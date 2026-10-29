use super::{capture, report, SessionEvent};
use crate::session::*;
use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec, vec::Vec};
use core::future::{pending, Future};
use std::sync::Mutex;

type Seen = Arc<Mutex<Vec<(&'static str, u64)>>>;
fn observer() -> (capture::Observer, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let out = seen.clone();
    let observer = Arc::new(move |event: &SessionEvent<'_>| {
        let entry = match event {
            SessionEvent::HandshakeReset {
                ended_subscriptions,
            } => ("reset", *ended_subscriptions as u64),
            SessionEvent::SubscribeRefused { id } => {
                assert_eq!(*id, "1");
                ("refused", 1)
            }
            SessionEvent::SubscriptionEnded { id } => {
                assert_eq!(*id, "2");
                ("ended", 2)
            }
            SessionEvent::SnapshotEncodeFailed { id, topic, error } => {
                assert_eq!((*id, *topic), ("3", "snapshot"));
                assert_eq!(**error, CodecError::Malformed);
                ("snapshot", 3)
            }
            SessionEvent::AcceptFailed { error } => {
                assert_eq!(*error, TransportError::Io);
                ("failed", 1)
            }
            SessionEvent::AcceptStillFailing { attempts, error } => {
                assert_eq!(*error, TransportError::Io);
                ("failing", *attempts)
            }
            SessionEvent::AcceptRecovered { after } => ("recovered", *after),
            SessionEvent::ConnectionLimitReached { limit } => ("limit", *limit as u64),
            SessionEvent::ListenerClosed { open_connections } => {
                ("closed", *open_connections as u64)
            }
            SessionEvent::AuthenticationRejected { error } => {
                assert_eq!(**error, AuthError::Unauthorized);
                ("auth", 0)
            }
            SessionEvent::DialFailed { error } => {
                assert_eq!(**error, TransportError::Io);
                ("dial", 0)
            }
            SessionEvent::ReconnectExhausted { attempts } => ("exhausted", *attempts as u64),
            _ => panic!("unexpected event: {event:?}"),
        };
        out.lock().unwrap().push(entry);
    });
    (observer, seen)
}

struct ScriptConnection {
    frames: VecDeque<u8>,
    peer: PeerInfo,
}
impl ScriptConnection {
    fn boxed(frames: &[u8]) -> Box<dyn Connection> {
        Box::new(Self {
            frames: frames.iter().copied().collect(),
            peer: PeerInfo::default(),
        })
    }
}
impl Connection for ScriptConnection {
    fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
        Box::pin(async move {
            // Give subscription pumps a turn between scripted inbound frames.
            tokio::task::yield_now().await;
            Ok(self.frames.pop_front().map(|b| vec![b]))
        })
    }
    fn send<'a>(&'a mut self, _: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
        Box::pin(async { Ok(()) })
    }
    fn peer(&self) -> &PeerInfo {
        &self.peer
    }
}
struct ScriptCodec;
impl EnvelopeCodec for ScriptCodec {
    fn decode(&self, frame: &[u8]) -> Result<Inbound, CodecError> {
        Ok(match frame[0] {
            b'h' => Inbound::Request {
                id: 0,
                method: "hello".into(),
                params: Arc::from([]),
            },
            b's' => Inbound::Subscribe {
                id: 1,
                topic: "live".into(),
            },
            b'e' => Inbound::Subscribe {
                id: 2,
                topic: "empty".into(),
            },
            b'x' => Inbound::Subscribe {
                id: 3,
                topic: "snapshot".into(),
            },
            _ => return Err(CodecError::Malformed),
        })
    }
    fn encode(&self, msg: Outbound<'_>, _: &mut Vec<u8>) -> Result<(), CodecError> {
        if matches!(msg, Outbound::Snapshot { .. }) {
            Err(CodecError::Malformed)
        } else {
            Ok(())
        }
    }
    fn encode_inbound(&self, _: Inbound, _: &mut Vec<u8>) -> Result<(), CodecError> {
        Ok(())
    }
    fn decode_outbound<'a>(&self, _: &'a [u8]) -> Result<Outbound<'a>, CodecError> {
        Err(CodecError::Malformed)
    }
}
struct ScriptDispatch {
    reject: bool,
}
impl Dispatch for ScriptDispatch {
    fn authenticate<'a>(
        &'a self,
        _: &'a PeerInfo,
        _: Option<&'a [u8]>,
    ) -> BoxFut<'a, Result<SessionCtx, AuthError>> {
        Box::pin(async move {
            if self.reject {
                Err(AuthError::Unauthorized)
            } else {
                Ok(SessionCtx::default())
            }
        })
    }
    fn open(&self, _: &SessionCtx) -> Box<dyn Session> {
        Box::new(ScriptSession)
    }
}
struct ScriptSession;
impl Session for ScriptSession {
    fn call<'a>(
        &'a mut self,
        _: &'a str,
        params: Payload,
    ) -> BoxFut<'a, Result<Payload, RpcError>> {
        Box::pin(async { Ok(params) })
    }
    fn starts_session(&self, method: &str) -> bool {
        method == "hello"
    }
    fn subscribe<'a>(
        &'a mut self,
        topic: &'a str,
    ) -> BoxFut<'a, Result<BoxStream<'static, SubUpdate>, RpcError>> {
        Box::pin(async move {
            let stream: BoxStream<'static, SubUpdate> = if topic == "empty" {
                Box::pin(futures_util::stream::empty())
            } else {
                Box::pin(futures_util::stream::pending())
            };
            Ok(stream)
        })
    }
    fn snapshots(&mut self, topic: &str) -> Vec<(alloc::string::String, Payload)> {
        if topic == "snapshot" {
            vec![(topic.into(), Arc::from([]))]
        } else {
            Vec::new()
        }
    }
    fn write<'a>(&'a mut self, _: &'a str, _: Payload) -> BoxFut<'a, Result<(), RpcError>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn server_emits_domain_events_at_the_existing_decisions() {
    let (observer, seen) = observer();
    capture::future(
        observer,
        run_session(
            ScriptConnection::boxed(b"hsshex"),
            &ScriptCodec,
            &ScriptDispatch { reject: false },
            &SessionConfig::default(),
        ),
    )
    .await;
    assert_eq!(
        *seen.lock().unwrap(),
        [("refused", 1), ("reset", 1), ("ended", 2), ("snapshot", 3)]
    );
}

#[tokio::test]
async fn authentication_rejection_is_captured_without_logging_features() {
    let (observer, seen) = observer();
    capture::future(
        observer,
        run_session(
            ScriptConnection::boxed(b""),
            &ScriptCodec,
            &ScriptDispatch { reject: true },
            &SessionConfig::default(),
        ),
    )
    .await;
    assert_eq!(*seen.lock().unwrap(), [("auth", 0)]);
}

struct ScriptListener(VecDeque<TransportResult<Box<dyn Connection>>>);
impl Listener for ScriptListener {
    fn accept(&mut self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async move { self.0.pop_front().unwrap_or(Err(TransportError::Closed)) })
    }
}
#[tokio::test]
async fn accept_loop_reports_throttled_failures_recovery_limit_and_closure() {
    let mut script: VecDeque<_> = (0..1000).map(|_| Err(TransportError::Io)).collect();
    script.push_back(Ok(ScriptConnection::boxed(b"")));
    script.push_back(Err(TransportError::Io));
    let config = SessionConfig {
        limits: SessionLimits {
            max_connections: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    let (observer, seen) = observer();
    capture::future(
        observer,
        serve(
            ScriptListener(script),
            Arc::new(ScriptCodec),
            Arc::new(ScriptDispatch { reject: false }),
            config,
        ),
    )
    .await;
    assert_eq!(
        *seen.lock().unwrap(),
        [
            ("failed", 1),
            ("failing", 10),
            ("failing", 100),
            ("failing", 1000),
            ("recovered", 1000),
            ("limit", 0),
            ("failed", 1),
            ("closed", 0)
        ]
    );
}

#[tokio::test]
async fn listener_closure_reports_the_connections_it_drains() {
    let (observer, seen) = observer();
    let listener = ScriptListener(VecDeque::from([Ok(ScriptConnection::boxed(b""))]));
    capture::future(
        observer,
        serve(
            listener,
            Arc::new(ScriptCodec),
            Arc::new(ScriptDispatch { reject: false }),
            SessionConfig::default(),
        ),
    )
    .await;
    assert_eq!(*seen.lock().unwrap(), [("closed", 1)]);
}

struct FailingDialer;
impl Dialer for FailingDialer {
    fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async { Err(TransportError::Io) })
    }
}
#[tokio::test]
async fn client_reports_dial_failure_and_reconnect_exhaustion() {
    let (observer, seen) = observer();
    let (_handle, engine) = run_client(
        FailingDialer,
        ScriptCodec,
        ClientConfig {
            max_reconnect_attempts: 1,
            reconnect: true,
            ..Default::default()
        },
        Arc::new(crate::executor::test_support::NoopRuntimeOps),
    );
    capture::future(observer, engine).await;
    assert_eq!(*seen.lock().unwrap(), [("dial", 0), ("exhausted", 1)]);
}

#[test]
fn capture_restores_nested_observers_and_unwinds() {
    let (outer, outer_seen) = observer();
    let (inner, inner_seen) = observer();
    capture::with(outer, || {
        report(SessionEvent::AcceptRecovered { after: 1 });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            capture::with(inner, || {
                report(SessionEvent::AcceptRecovered { after: 2 });
                panic!("test unwind");
            })
        }));
        assert!(result.is_err());
        report(SessionEvent::AcceptRecovered { after: 3 });
    });
    report(SessionEvent::AcceptRecovered { after: 4 });
    assert_eq!(
        *outer_seen.lock().unwrap(),
        [("recovered", 1), ("recovered", 3)]
    );
    assert_eq!(*inner_seen.lock().unwrap(), [("recovered", 2)]);
}

#[tokio::test]
async fn capture_is_scoped_to_each_poll_and_isolates_concurrent_futures() {
    let (a, a_seen) = observer();
    let (b, b_seen) = observer();
    let task = |n| async move {
        report(SessionEvent::AcceptRecovered { after: n });
        tokio::task::yield_now().await;
        report(SessionEvent::AcceptRecovered { after: n + 1 });
    };
    futures_util::future::join(
        capture::future(a.clone(), task(1)),
        capture::future(b, task(10)),
    )
    .await;
    let mut parked = Box::pin(capture::future(a, pending::<()>()));
    core::future::poll_fn(|cx| {
        assert!(parked.as_mut().poll(cx).is_pending());
        core::task::Poll::Ready(())
    })
    .await;
    report(SessionEvent::AcceptRecovered { after: 100 });
    drop(parked);
    assert_eq!(
        *a_seen.lock().unwrap(),
        [("recovered", 1), ("recovered", 2)]
    );
    assert_eq!(
        *b_seen.lock().unwrap(),
        [("recovered", 10), ("recovered", 11)]
    );
}

#[cfg(feature = "log")]
mod backend_tests {
    use super::*;

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
            assert_eq!(event.metadata().target(), "aimdb_core::session::server");
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    #[test]
    fn log_backend_preserves_text_levels_and_the_raising_module_target() {
        // Scope collection to this thread so parallel engine tests cannot contaminate it.
        std::thread_local! {
            static RECORDS: std::cell::RefCell<Option<Vec<(log::Level, alloc::string::String, alloc::string::String)>>> = const { std::cell::RefCell::new(None) };
        }
        struct Logger;
        impl log::Log for Logger {
            fn enabled(&self, _: &log::Metadata<'_>) -> bool {
                true
            }
            fn log(&self, record: &log::Record<'_>) {
                RECORDS.with(|records| {
                    if let Some(records) = records.borrow_mut().as_mut() {
                        records.push((
                            record.level(),
                            record.target().into(),
                            alloc::format!("{}", record.args()),
                        ));
                    }
                });
            }
            fn flush(&self) {}
        }
        static LOGGER: Logger = Logger;
        log::set_logger(&LOGGER).expect("no other unit test installs a logger");
        log::set_max_level(log::LevelFilter::Trace);
        RECORDS.with(|records| {
            records.replace(Some(Vec::new()));
        });
        #[cfg(feature = "tracing")]
        let trace_seen = Arc::new(core::sync::atomic::AtomicUsize::new(0));
        let emit = || {
            report(SessionEvent::AcceptFailed {
                error: TransportError::Io,
            });
            report(SessionEvent::AcceptStillFailing {
                attempts: 10,
                error: TransportError::Busy,
            });
            report(SessionEvent::AcceptRecovered { after: 12 });
        };
        // A real subscriber prevents tracing's optional log fallback from
        // duplicating the facade's explicit log destination.
        #[cfg(feature = "tracing")]
        tracing::subscriber::with_default(TraceSink(trace_seen.clone()), emit);
        #[cfg(not(feature = "tracing"))]
        emit();
        #[cfg(feature = "tracing")]
        assert_eq!(trace_seen.load(core::sync::atomic::Ordering::Relaxed), 3);
        let records = RECORDS.with(|records| records.take().unwrap());
        assert_eq!(
            records,
            [
                (
                    log::Level::Error,
                    "aimdb_core::session::server".into(),
                    "accept failed: Io; retrying, further failures are summarized".into()
                ),
                (
                    log::Level::Warn,
                    "aimdb_core::session::server".into(),
                    "accept still failing: 10 attempts in a row, last: Busy".into()
                ),
                (
                    log::Level::Info,
                    "aimdb_core::session::server".into(),
                    "accept recovered after 12 failed attempt(s)".into()
                ),
            ]
        );
    }
}
