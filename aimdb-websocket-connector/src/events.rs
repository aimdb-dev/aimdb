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
mod backend_tests {
    use super::*;
    use aimdb_core::__private::log;
    use std::{cell::RefCell, sync::Mutex};
    std::thread_local! {
        static RECORDS: RefCell<Option<Vec<(log::Level, String, String)>>> = const { RefCell::new(None) };
    }
    struct Logger;
    impl log::Log for Logger {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            RECORDS.with(|slot| {
                if let Some(records) = slot.borrow_mut().as_mut() {
                    records.push((
                        record.level(),
                        record.target().into(),
                        record.args().to_string(),
                    ));
                }
            });
        }
        fn flush(&self) {}
    }
    #[cfg(feature = "tracing")]
    struct TraceSink(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    #[cfg(feature = "tracing")]
    impl tracing::Subscriber for TraceSink {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            assert_eq!(
                event.metadata().target(),
                "aimdb_websocket_connector::events"
            );
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    #[test]
    fn facade_delivery_preserves_message_level_and_target() {
        static LOGGER: Logger = Logger;
        log::set_logger(&LOGGER).unwrap();
        log::set_max_level(log::LevelFilter::Trace);
        RECORDS.with(|slot| {
            slot.replace(Some(Vec::new()));
        });
        let seen = std::sync::Arc::new(Mutex::new(0));
        let observer = {
            let seen = seen.clone();
            std::sync::Arc::new(move |event: &WebSocketEvent<'_>| {
                assert!(matches!(event, WebSocketEvent::BindFailed { .. }));
                *seen.lock().unwrap() += 1;
            })
        };
        let error = std::io::Error::other("occupied");
        #[cfg(feature = "tracing")]
        let trace_seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let emit = || {
            capture::with(observer, || {
                report(WebSocketEvent::BindFailed {
                    address: "127.0.0.1:1234".parse().unwrap(),
                    error: &error,
                })
            });
        };
        // Install a real subscriber so tracing's optional log compatibility
        // fallback cannot duplicate the explicit facade log destination.
        #[cfg(feature = "tracing")]
        tracing::subscriber::with_default(TraceSink(trace_seen.clone()), emit);
        #[cfg(not(feature = "tracing"))]
        emit();
        #[cfg(feature = "tracing")]
        assert_eq!(trace_seen.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(*seen.lock().unwrap(), 1);
        let records = RECORDS.with(|slot| slot.take().unwrap());
        assert_eq!(
            records,
            [(
                log::Level::Error,
                "aimdb_websocket_connector::events".into(),
                "WebSocket connector failed to bind 127.0.0.1:1234: occupied".into()
            )]
        );
    }
}
