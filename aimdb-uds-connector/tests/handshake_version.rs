//! End-to-end handshake version gate (design 048 WI1).
//!
//! The AimX server must refuse a client whose declared protocol version is
//! major-incompatible — or absent — rather than completing `hello` into a
//! `welcome`/`reply` shape the client can't parse and letting it panic on its
//! first `record.query`. These drive the real [`AimxDispatch`] `hello` handler
//! through the [`Session`] surface (no socket needed) and assert the refusal.

use std::sync::Arc;

use aimdb_core::remote::{AimxConfig, PROTOCOL_VERSION};
use aimdb_core::session::aimx::AimxDispatch;
use aimdb_core::session::{Dispatch, Payload, RpcError, Session, SessionCtx};
use aimdb_core::AimDbBuilder;
use aimdb_tokio_adapter::TokioAdapter;
use serde_json::json;

/// A dispatch session over an empty db — enough to exercise the handshake, which
/// gates on the declared version before it ever touches the record set.
async fn open_session() -> Box<dyn Session> {
    let (db, _runner) = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .build()
        .await
        .expect("build empty db");
    let dispatch = AimxDispatch::new(Arc::new(db), AimxConfig::uds_default());
    dispatch.open(&SessionCtx::default())
}

fn hello_payload(value: serde_json::Value) -> Payload {
    Arc::from(serde_json::to_vec(&value).unwrap().as_slice())
}

#[tokio::test]
async fn current_version_completes_handshake() {
    let mut session = open_session().await;
    let reply = session
        .call(
            "hello",
            hello_payload(json!({ "client": "test", "version": PROTOCOL_VERSION })),
        )
        .await
        .expect("current-version hello should be welcomed");
    let welcome: serde_json::Value = serde_json::from_slice(&reply).unwrap();
    assert_eq!(welcome["version"], PROTOCOL_VERSION);
}

#[tokio::test]
async fn legacy_version_is_refused() {
    let mut session = open_session().await;
    let err = session
        .call(
            "hello",
            hello_payload(json!({ "client": "test", "version": "2.0" })),
        )
        .await
        .expect_err("a pre-3.x client must be refused at the handshake");
    assert_eq!(err, RpcError::VersionMismatch);
}

#[tokio::test]
async fn missing_version_is_refused() {
    let mut session = open_session().await;
    let err = session
        .call("hello", hello_payload(json!({ "client": "test" })))
        .await
        .expect_err("a version-less hello must be refused (fail closed)");
    assert_eq!(err, RpcError::VersionMismatch);
}
