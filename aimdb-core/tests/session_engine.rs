//! Phase 2 exit criterion (doc 036 / issue.md): a `serve` server and a
//! `run_client` client engine, talking over a throwaway in-memory pipe,
//! round-trip **RPC + a streaming subscription + a fire-and-forget write** in
//! both directions — proving the shared substrate (`Connection` /
//! `EnvelopeCodec` / `Inbound`/`Outbound`) is genuinely role-neutral.
//!
//! The substrate here is deliberately throwaway: a channel-backed `Connection`
//! (framing-in-transport: one `Vec<u8>` per logical frame), a `Listener`/
//! `Dialer` pair over a connect channel, a tiny line-oriented `EnvelopeCodec`,
//! and an echo `Dispatch`. The real UDS/NDJSON/AimX impls land in Phase 3.

#![cfg(feature = "connector-session")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;

use aimdb_core::session::{
    run_client, serve, AuthError, BoxFut, BoxStream, ClientConfig, CodecError, Connection, Dialer,
    Dispatch, EnvelopeCodec, Inbound, Listener, Outbound, Payload, PeerInfo, RpcError,
    SessionConfig, SessionCtx, TransportError, TransportResult,
};

// ===========================================================================
// Channel-backed transport (Layer 1)
// ===========================================================================

/// A framed bidirectional pipe: send to the peer, receive from the peer. One
/// `Vec<u8>` == one logical frame (framing lives in the transport).
struct ChannelConn {
    tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    peer: PeerInfo,
}

fn conn_pair() -> (ChannelConn, ChannelConn) {
    let (a_tx, a_rx) = tokio::sync::mpsc::unbounded_channel();
    let (b_tx, b_rx) = tokio::sync::mpsc::unbounded_channel();
    (
        ChannelConn {
            tx: a_tx,
            rx: b_rx,
            peer: PeerInfo::default(),
        },
        ChannelConn {
            tx: b_tx,
            rx: a_rx,
            peer: PeerInfo::default(),
        },
    )
}

impl Connection for ChannelConn {
    fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
        Box::pin(async move { Ok(self.rx.recv().await) }) // None == peer closed
    }
    fn send<'a>(&'a mut self, frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
        let tx = self.tx.clone();
        let bytes = frame.to_vec();
        Box::pin(async move { tx.send(bytes).map_err(|_| TransportError::Closed) })
    }
    fn peer(&self) -> &PeerInfo {
        &self.peer
    }
}

struct ChannelListener {
    incoming: tokio::sync::mpsc::UnboundedReceiver<Box<dyn Connection>>,
}

impl Listener for ChannelListener {
    fn accept(&mut self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async move { self.incoming.recv().await.ok_or(TransportError::Closed) })
    }
}

struct ChannelDialer {
    connect_tx: tokio::sync::mpsc::UnboundedSender<Box<dyn Connection>>,
}

impl Dialer for ChannelDialer {
    fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        let connect_tx = self.connect_tx.clone();
        Box::pin(async move {
            let (server_side, client_side) = conn_pair();
            connect_tx
                .send(Box::new(server_side) as Box<dyn Connection>)
                .map_err(|_| TransportError::Closed)?;
            Ok(Box::new(client_side) as Box<dyn Connection>)
        })
    }
}

fn transport_pair() -> (ChannelListener, ChannelDialer) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (
        ChannelListener { incoming: rx },
        ChannelDialer { connect_tx: tx },
    )
}

// ===========================================================================
// Tiny line-oriented EnvelopeCodec (symmetric: both engine directions)
// ===========================================================================

struct LineCodec;

fn payload_from(s: &str) -> Payload {
    Arc::from(s.as_bytes())
}

fn utf8(b: &[u8]) -> Result<&str, CodecError> {
    std::str::from_utf8(b).map_err(|_| CodecError::Malformed)
}

fn rpc_code(e: &RpcError) -> &'static str {
    match e {
        RpcError::NotFound => "notfound",
        RpcError::Denied => "denied",
        _ => "internal",
    }
}

fn code_rpc(s: &str) -> RpcError {
    match s {
        "notfound" => RpcError::NotFound,
        "denied" => RpcError::Denied,
        _ => RpcError::Internal,
    }
}

impl EnvelopeCodec for LineCodec {
    // --- server direction --------------------------------------------------
    fn decode(&self, frame: &[u8]) -> Result<Inbound, CodecError> {
        let s = utf8(frame)?;
        let (tag, rest) = s.split_once('\n').unwrap_or((s, ""));
        match tag {
            "REQ" => {
                let (id, r) = rest.split_once('\n').ok_or(CodecError::Malformed)?;
                let (method, params) = r.split_once('\n').unwrap_or((r, ""));
                Ok(Inbound::Request {
                    id: id.parse().map_err(|_| CodecError::Malformed)?,
                    method: method.to_string(),
                    params: payload_from(params),
                })
            }
            "SUB" => {
                let (id, topic) = rest.split_once('\n').ok_or(CodecError::Malformed)?;
                Ok(Inbound::Subscribe {
                    id: id.parse().map_err(|_| CodecError::Malformed)?,
                    topic: topic.to_string(),
                })
            }
            "UNSUB" => Ok(Inbound::Unsubscribe {
                sub: rest.to_string(),
            }),
            "WRITE" => {
                let (topic, payload) = rest.split_once('\n').unwrap_or((rest, ""));
                Ok(Inbound::Write {
                    topic: topic.to_string(),
                    payload: payload_from(payload),
                })
            }
            "PING" => Ok(Inbound::Ping),
            _ => Err(CodecError::Malformed),
        }
    }

    fn encode(&self, msg: Outbound<'_>, out: &mut Vec<u8>) -> Result<(), CodecError> {
        let s = match msg {
            Outbound::Reply { id, result } => match result {
                Ok(data) => format!("REPLY\n{}\nOK\n{}", id, utf8(&data)?),
                Err(e) => format!("REPLY\n{}\nERR\n{}", id, rpc_code(&e)),
            },
            Outbound::Event { sub, seq, data } => {
                format!("EVENT\n{}\n{}\n{}", sub, seq, utf8(&data)?)
            }
            Outbound::Snapshot { topic, data } => format!("SNAP\n{}\n{}", topic, utf8(&data)?),
            Outbound::Pong => "PONG".to_string(),
        };
        out.extend_from_slice(s.as_bytes());
        Ok(())
    }

    // --- client direction (Phase 2 dual) -----------------------------------
    fn encode_inbound(&self, msg: Inbound, out: &mut Vec<u8>) -> Result<(), CodecError> {
        let s = match msg {
            Inbound::Request { id, method, params } => {
                format!("REQ\n{}\n{}\n{}", id, method, utf8(&params)?)
            }
            Inbound::Subscribe { id, topic } => format!("SUB\n{}\n{}", id, topic),
            Inbound::Unsubscribe { sub } => format!("UNSUB\n{}", sub),
            Inbound::Write { topic, payload } => format!("WRITE\n{}\n{}", topic, utf8(&payload)?),
            Inbound::Ping => "PING".to_string(),
        };
        out.extend_from_slice(s.as_bytes());
        Ok(())
    }

    fn decode_outbound<'a>(&self, frame: &'a [u8]) -> Result<Outbound<'a>, CodecError> {
        let s = utf8(frame)?;
        let (tag, rest) = s.split_once('\n').unwrap_or((s, ""));
        match tag {
            "REPLY" => {
                let (id, r) = rest.split_once('\n').ok_or(CodecError::Malformed)?;
                let (kind, tail) = r.split_once('\n').unwrap_or((r, ""));
                let result = match kind {
                    "OK" => Ok(payload_from(tail)),
                    "ERR" => Err(code_rpc(tail)),
                    _ => return Err(CodecError::Malformed),
                };
                Ok(Outbound::Reply {
                    id: id.parse().map_err(|_| CodecError::Malformed)?,
                    result,
                })
            }
            "EVENT" => {
                let (sub, r) = rest.split_once('\n').ok_or(CodecError::Malformed)?;
                let (seq, data) = r.split_once('\n').unwrap_or((r, ""));
                Ok(Outbound::Event {
                    sub,
                    seq: seq.parse().map_err(|_| CodecError::Malformed)?,
                    data: payload_from(data),
                })
            }
            "SNAP" => {
                let (topic, data) = rest.split_once('\n').unwrap_or((rest, ""));
                Ok(Outbound::Snapshot {
                    topic,
                    data: payload_from(data),
                })
            }
            "PONG" => Ok(Outbound::Pong),
            _ => Err(CodecError::Malformed),
        }
    }
}

// ===========================================================================
// Echo dispatch (Layer 3)
// ===========================================================================

/// Shared log of `(topic, payload)` writes the server received, for assertion.
type WriteLog = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

struct EchoDispatch {
    writes: WriteLog,
}

impl Dispatch for EchoDispatch {
    fn authenticate<'a>(
        &'a self,
        _peer: &'a PeerInfo,
        _first: Option<&'a [u8]>,
    ) -> BoxFut<'a, Result<SessionCtx, AuthError>> {
        Box::pin(async { Ok(SessionCtx::default()) })
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a SessionCtx,
        _method: &'a str,
        params: Payload,
    ) -> BoxFut<'a, Result<Payload, RpcError>> {
        // Echo the params straight back.
        Box::pin(async move { Ok(params) })
    }

    fn subscribe(
        &self,
        _ctx: &SessionCtx,
        topic: &str,
    ) -> Result<BoxStream<'static, Payload>, RpcError> {
        // Three synthetic updates derived from the topic, then end.
        let items: Vec<Payload> = (1..=3)
            .map(|i| payload_from(&format!("{topic}#{i}")))
            .collect();
        Ok(Box::pin(futures::stream::iter(items)))
    }

    fn write<'a>(
        &'a self,
        _ctx: &'a SessionCtx,
        topic: &'a str,
        payload: Payload,
    ) -> BoxFut<'a, Result<(), RpcError>> {
        let writes = self.writes.clone();
        let topic = topic.to_string();
        Box::pin(async move {
            writes.lock().unwrap().push((topic, payload.to_vec()));
            Ok(())
        })
    }
}

// ===========================================================================
// The exit-criterion test
// ===========================================================================

#[tokio::test]
async fn echo_roundtrip_rpc_streaming_and_write() {
    let (listener, dialer) = transport_pair();
    let writes = Arc::new(Mutex::new(Vec::new()));
    let dispatch = Arc::new(EchoDispatch {
        writes: writes.clone(),
    });

    // Server engine on the runner's stand-in (a task — the engine itself is
    // spawn-free; the test harness drives the one returned future).
    let server = tokio::spawn(serve(
        listener,
        Arc::new(LineCodec),
        dispatch,
        SessionConfig::default(),
    ));

    // Client engine: handshake-as-caller (Ping/Pong), no reconnect for the test.
    let (handle, client_fut) = run_client(
        dialer,
        LineCodec,
        ClientConfig {
            reconnect: false,
            reconnect_delay: Duration::from_millis(10),
            sends_hello: true,
        },
    );
    let client = tokio::spawn(client_fut);

    // 1) RPC: one request → one reply (echo).
    let reply = handle.call("echo", payload_from("hello")).await.unwrap();
    assert_eq!(&*reply, b"hello", "RPC reply should echo the params");

    // 2) Streaming: subscribe → three events routed back by sub id.
    let mut stream = handle.subscribe("temp").unwrap();
    let e1 = stream.next().await.expect("event 1");
    let e2 = stream.next().await.expect("event 2");
    let e3 = stream.next().await.expect("event 3");
    assert_eq!(&*e1, b"temp#1");
    assert_eq!(&*e2, b"temp#2");
    assert_eq!(&*e3, b"temp#3");

    // 3) Fire-and-forget write, then a follow-up RPC. FIFO on the single
    //    connection guarantees the write frame is processed before the reply
    //    returns, so the write is observable by the time the call resolves.
    handle.write("room", payload_from("on")).unwrap();
    let _ = handle.call("noop", payload_from("x")).await.unwrap();
    let got = writes.lock().unwrap().clone();
    assert_eq!(
        got,
        vec![("room".to_string(), b"on".to_vec())],
        "server should have received the write"
    );

    // Teardown: dropping the only handle stops the client engine gracefully;
    // the server loop is unbounded, so abort it.
    drop(handle);
    drop(stream);
    client
        .await
        .expect("client engine should stop cleanly when handles drop");
    server.abort();
}
