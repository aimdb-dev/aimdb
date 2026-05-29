//! Phase 3 client-first exit criterion: the engine-based [`AimxConnection`]
//! round-trips the AimX-v2 wire — `hello` handshake, RPC (`record.get`/`set`),
//! a streaming subscription, and a fire-and-forget write — against a `serve`
//! engine test-server over a **real Unix-domain socket**.
//!
//! The server side uses the production [`AimxCodec`] + [`UdsConnection`]; the
//! only test-local pieces are a `UdsListener` (the accepting transport half,
//! deferred from core to the server port) and a small echo-ish `Dispatch`. This
//! proves the reshaped wire end-to-end before the real server `Dispatch` exists.

use std::sync::{Arc, Mutex};

use aimdb_client::AimxConnection;
use aimdb_core::session::aimx::{AimxCodec, UdsConnection};
use aimdb_core::session::{
    serve, AuthError, BoxFut, BoxStream, Connection, Dispatch, Listener, Payload, PeerInfo,
    RpcError, SessionConfig, SessionCtx, TransportError, TransportResult,
};
use serde_json::json;
use tokio::net::UnixListener;

// ---------------------------------------------------------------------------
// Test-local accepting transport (the `UdsListener` core gains in the server port)
// ---------------------------------------------------------------------------

struct TestUdsListener {
    inner: UnixListener,
}

impl Listener for TestUdsListener {
    fn accept(&mut self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async move {
            let (stream, _addr) = self.inner.accept().await.map_err(|_| TransportError::Io)?;
            Ok(Box::new(UdsConnection::new(stream)) as Box<dyn Connection>)
        })
    }
}

// ---------------------------------------------------------------------------
// Minimal AimX dispatch (stand-in for the real server `Dispatch`, server port)
// ---------------------------------------------------------------------------

type WriteLog = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

struct TestDispatch {
    writes: WriteLog,
}

fn payload(v: serde_json::Value) -> Payload {
    Payload::from(serde_json::to_vec(&v).unwrap().as_slice())
}

impl Dispatch for TestDispatch {
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
        method: &'a str,
        params: Payload,
    ) -> BoxFut<'a, Result<Payload, RpcError>> {
        let method = method.to_string();
        Box::pin(async move {
            match method.as_str() {
                "hello" => Ok(payload(json!({
                    "version": "2.0",
                    "server": "test",
                    "permissions": ["read", "write"],
                    "writable_records": ["temp"],
                }))),
                "record.list" => Ok(payload(json!([{ "name": "temp", "writable": true }]))),
                "record.get" => {
                    // Echo the requested name back as a fixed value.
                    let v: serde_json::Value = serde_json::from_slice(&params).unwrap_or_default();
                    let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    if name == "temp" {
                        Ok(payload(json!(42)))
                    } else {
                        Err(RpcError::NotFound)
                    }
                }
                "record.set" => Ok(payload(json!({ "ok": true }))),
                _ => Err(RpcError::NotFound),
            }
        })
    }

    fn subscribe(
        &self,
        _ctx: &SessionCtx,
        topic: &str,
    ) -> Result<BoxStream<'static, Payload>, RpcError> {
        // Three synthetic updates derived from the topic, then end.
        let items: Vec<Payload> = (1..=3)
            .map(|i| payload(json!({ "topic": topic, "n": i })))
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

// ---------------------------------------------------------------------------
// The exit-criterion test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn aimx_client_roundtrip_over_uds() {
    use futures::StreamExt;

    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("aimdb.sock");

    // Bind before connecting so the client's dial always finds the socket.
    let listener = TestUdsListener {
        inner: UnixListener::bind(&sock).unwrap(),
    };
    let writes: WriteLog = Arc::new(Mutex::new(Vec::new()));
    let dispatch = Arc::new(TestDispatch {
        writes: writes.clone(),
    });
    let server = tokio::spawn(serve(
        listener,
        Arc::new(AimxCodec),
        dispatch,
        SessionConfig::default(),
    ));

    // Connect: this performs the `hello` handshake and captures the Welcome.
    let conn = AimxConnection::connect(&sock).await.expect("connect");
    assert_eq!(conn.server_info().server, "test");
    assert!(conn
        .server_info()
        .permissions
        .contains(&"write".to_string()));

    // RPC: record.get
    let temp = conn.get_record("temp").await.expect("get temp");
    assert_eq!(temp, json!(42));

    // RPC: record.get on a missing record maps to a server error.
    assert!(conn.get_record("missing").await.is_err());

    // RPC: record.set
    let set = conn.set_record("temp", json!(7)).await.expect("set temp");
    assert_eq!(set, json!({ "ok": true }));

    // Streaming subscription: three events routed back by the engine-owned id.
    let mut stream = conn.subscribe("temp").expect("subscribe");
    for n in 1..=3 {
        let ev = stream.next().await.expect("event");
        assert_eq!(ev, json!({ "topic": "temp", "n": n }));
    }

    // Fire-and-forget write, then a follow-up RPC. FIFO over the single
    // connection guarantees the write is processed before the reply returns.
    conn.write_record("temp", json!("on")).expect("write");
    let _ = conn.get_record("temp").await.expect("get after write");
    let got = writes.lock().unwrap().clone();
    assert_eq!(
        got.len(),
        1,
        "server should have received exactly one write"
    );
    assert_eq!(got[0].0, "temp");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&got[0].1).unwrap(),
        json!({ "value": "on" })
    );

    drop(conn); // stops the client engine
    server.abort();
}
