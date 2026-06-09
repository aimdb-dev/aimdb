//! Host smoke for the centralized Embassy connector spine — the **server** side
//! the serial smoke test doesn't reach: [`OneShotCell`]'s take-once semantics,
//! [`OneShotListener`]'s park-forever second `accept`, and core's `serve` over
//! the one-shot listener — the exact path `EmbassySessionServer::build` (and the
//! serial `SerialServer`) drives on an MCU.
//!
//! Runs under the adapter's host test feature set (`alloc,…,connectors`); the
//! futures are driven by `futures::executor::block_on`, no executor needed.

#![cfg(all(not(feature = "std"), feature = "connectors"))]

use std::sync::{Arc, Mutex};

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use futures::executor::block_on;
use futures::future::{select, Either};
use futures::pin_mut;

use aimdb_core::session::{
    serve, AuthError, BoxFut, CodecError, Connection, Dispatch, EnvelopeCodec, Inbound, Listener,
    Outbound, Payload, PeerInfo, RpcError, Session, SessionConfig, SessionCtx, SessionLimits,
    TransportResult,
};
use aimdb_embassy_adapter::connectors::{OneShotCell, OneShotListener};

/// Yields (self-waking) `n` times, then completes — bounds how long we drive a
/// never-returning future like `serve` under `block_on`.
struct YieldN(usize);

impl Future for YieldN {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 == 0 {
            Poll::Ready(())
        } else {
            self.0 -= 1;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

/// A scripted connection: `recv` yields the queued frames then `None` (EOF);
/// `send` records every frame into the shared log.
struct ScriptedConn {
    /// Frames to yield, in reverse order (popped from the back).
    inbox: Vec<Vec<u8>>,
    sent: Arc<Mutex<Vec<Vec<u8>>>>,
    peer: PeerInfo,
}

impl ScriptedConn {
    fn new(inbox: Vec<Vec<u8>>, sent: Arc<Mutex<Vec<Vec<u8>>>>) -> Self {
        Self {
            inbox,
            sent,
            peer: PeerInfo::default(),
        }
    }
}

impl Connection for ScriptedConn {
    fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
        Box::pin(async move { Ok(self.inbox.pop()) })
    }
    fn send<'a>(&'a mut self, frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
        self.sent.lock().unwrap().push(frame.to_vec());
        Box::pin(async { Ok(()) })
    }
    fn peer(&self) -> &PeerInfo {
        &self.peer
    }
}

/// Minimal server wire: a request frame is `[id:8][params]`; the reply echoes
/// the same shape back. Client-direction methods are unused here.
struct EchoCodec;

impl EnvelopeCodec for EchoCodec {
    fn decode(&self, frame: &[u8]) -> Result<Inbound, CodecError> {
        if frame.len() < 8 {
            return Err(CodecError::Malformed);
        }
        Ok(Inbound::Request {
            id: u64::from_be_bytes(frame[0..8].try_into().unwrap()),
            method: "echo".to_string(),
            params: Payload::from(&frame[8..]),
        })
    }
    fn encode(&self, msg: Outbound<'_>, out: &mut Vec<u8>) -> Result<(), CodecError> {
        match msg {
            Outbound::Reply {
                id,
                result: Ok(payload),
            } => {
                out.extend_from_slice(&id.to_be_bytes());
                out.extend_from_slice(&payload);
                Ok(())
            }
            _ => Err(CodecError::Malformed),
        }
    }
    fn encode_inbound(&self, _msg: Inbound, _out: &mut Vec<u8>) -> Result<(), CodecError> {
        Err(CodecError::Malformed)
    }
    fn decode_outbound<'a>(&self, _frame: &'a [u8]) -> Result<Outbound<'a>, CodecError> {
        Err(CodecError::Malformed)
    }
}

/// Accepts every peer; each session echoes a call's params back as the reply.
struct EchoDispatch;

impl Dispatch for EchoDispatch {
    fn authenticate<'a>(
        &'a self,
        _peer: &'a PeerInfo,
        _first: Option<&'a [u8]>,
    ) -> BoxFut<'a, Result<SessionCtx, AuthError>> {
        Box::pin(async { Ok(SessionCtx::default()) })
    }
    fn open(&self, _ctx: &SessionCtx) -> Box<dyn Session> {
        Box::new(EchoSession)
    }
}

struct EchoSession;

impl Session for EchoSession {
    fn call<'a>(
        &'a mut self,
        _method: &'a str,
        params: Payload,
    ) -> BoxFut<'a, Result<Payload, RpcError>> {
        Box::pin(async move { Ok(params) })
    }
    fn write<'a>(
        &'a mut self,
        _topic: &'a str,
        _payload: Payload,
    ) -> BoxFut<'a, Result<(), RpcError>> {
        Box::pin(async { Ok(()) })
    }
}

fn session_config() -> SessionConfig {
    SessionConfig {
        limits: SessionLimits {
            // The one-shot spine serves a single point-to-point peer.
            max_connections: 1,
            max_subs_per_connection: 4,
        },
        reads_hello: false,
        acks_subscribe: false,
    }
}

#[test]
fn one_shot_cell_hands_out_exactly_once() {
    let cell = OneShotCell::new(42u32);
    assert_eq!(cell.take(), Some(42));
    assert_eq!(cell.take(), None);

    let cell = OneShotCell::new("conn");
    assert_eq!(cell.take_required().unwrap(), "conn");
    // Second take: the canonical "already built" error, not a panic.
    assert!(cell.take_required().is_err());
}

#[test]
fn one_shot_listener_parks_after_first_accept() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let mut listener = OneShotListener::new(ScriptedConn::new(vec![], sent));

    block_on(async move {
        listener
            .accept()
            .await
            .expect("first accept hands out the connection");

        // Point-to-point: the second accept must park forever, not error — an
        // erroring accept would tear `serve` down.
        let second = listener.accept();
        pin_mut!(second);
        match select(second, YieldN(8)).await {
            Either::Left(_) => panic!("second accept must park forever"),
            Either::Right(((), _)) => {}
        }
    });
}

#[test]
fn serve_dispatches_over_one_shot_listener_then_keeps_waiting() {
    let sent = Arc::new(Mutex::new(Vec::new()));

    // One scripted request (`id=7`, params `ping`), then EOF.
    let mut request = 7u64.to_be_bytes().to_vec();
    request.extend_from_slice(b"ping");
    let conn = ScriptedConn::new(vec![request.clone()], sent.clone());

    let serve_fut = serve(
        OneShotListener::new(conn),
        Arc::new(EchoCodec),
        Arc::new(EchoDispatch),
        session_config(),
    );

    block_on(async move {
        pin_mut!(serve_fut);
        // `serve` must process the session but never return: after the peer's
        // EOF it loops back to the parked one-shot accept.
        match select(serve_fut, YieldN(8)).await {
            Either::Left(_) => panic!("serve must keep waiting on the parked accept"),
            Either::Right(((), _)) => {}
        }
    });

    // The echoed reply went out before EOF: same `[id:8][params]` bytes back.
    let sent = sent.lock().unwrap();
    assert_eq!(sent.as_slice(), &[request]);
}
