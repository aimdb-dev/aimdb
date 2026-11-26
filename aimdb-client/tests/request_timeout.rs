//! Requests after the handshake are bounded by the connection's request
//! timeout (#474): a silent instance fails a call with
//! [`ClientError::Timeout`] instead of hanging it.

#![cfg(feature = "transport-uds")]

use std::time::{Duration, Instant};

use aimdb_client::engine::DEFAULT_REQUEST_TIMEOUT;
use aimdb_client::{AimxConnection, ClientError};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

/// A server on the raw AimX wire that completes the `hello` handshake, answers
/// `record.list` with an empty list, and never answers anything else — an
/// instance that stopped responding after the handshake.
async fn serve_silent(dir: &tempfile::TempDir) -> String {
    let path = dir.path().join("silent.sock");
    let listener = UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let frame: Value = serde_json::from_str(&line).unwrap();
                    let ok = |body: Value| json!({ "t": "reply", "id": frame["id"], "ok": body });
                    let reply = match (frame["t"].as_str(), frame["method"].as_str()) {
                        (Some("ping"), _) => json!({ "t": "pong" }),
                        (Some("req"), Some("hello")) => ok(json!({
                            "version": "4.0", "server": "aimdb",
                            "permissions": ["read", "write"], "writable_records": [],
                        })),
                        (Some("req"), Some("record.list")) => ok(json!([])),
                        // Swallow the request: no reply, ever.
                        _ => continue,
                    };
                    let mut bytes = serde_json::to_vec(&reply).unwrap();
                    bytes.push(b'\n');
                    write.write_all(&bytes).await.unwrap();
                }
            });
        }
    });
    path.to_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_silent_instance_times_out_instead_of_hanging() {
    let dir = tempfile::tempdir().unwrap();
    let sock = serve_silent(&dir).await;
    let conn = AimxConnection::connect(&sock).await.expect("connect");
    assert_eq!(conn.request_timeout(), DEFAULT_REQUEST_TIMEOUT);

    let after = Duration::from_millis(200);
    let conn = conn.with_request_timeout(after);

    let started = Instant::now();
    let err = conn
        .set_record("spike.fault", json!({ "kind": 2 }))
        .await
        .expect_err("no reply must not hang");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "bounded by the request timeout, took {:?}",
        started.elapsed()
    );
    match &err {
        ClientError::Timeout { method, after: a } => {
            assert_eq!(method, "record.set");
            assert_eq!(*a, after);
        }
        other => panic!("expected a timeout, got {other}"),
    }
    assert!(err.to_string().contains("did not answer"), "{err}");

    // Every reply-awaiting call is bounded, including the ones that treat
    // `not_found` as an answer.
    for result in [
        conn.get_record("x").await.map(|_| ()),
        conn.instance_info().await.map(|_| ()),
        conn.post_link().await.map(|_| ()),
    ] {
        assert!(
            matches!(result, Err(ClientError::Timeout { .. })),
            "{result:?}"
        );
    }

    // A timeout leaves the connection open: a request the instance does
    // answer still succeeds.
    assert!(!conn.is_closed());
    assert!(conn.list_records().await.expect("still served").is_empty());
}
