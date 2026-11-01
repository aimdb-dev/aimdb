//! Public-API coverage for typed reads and custom polling routes.
#![cfg(feature = "server")]

use aimdb_core::{buffer::BufferCfg, AimDb, AimDbBuilder, DbError};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_websocket_connector::{
    AuthError, AuthHandler, AuthRequest, Permissions, WebSocketConnector,
};
use axum::{extract::State, http::StatusCode, routing::get, Extension, Router};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// No serde traits and no remote-access registration.
#[derive(Clone, Debug, PartialEq)]
struct Reading(i32);

#[tokio::test]
async fn typed_latest_preserves_lookup_and_buffer_semantics() {
    let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));
    for key in ["first", "second"] {
        builder.configure::<Reading>(key, |reg| {
            reg.buffer(BufferCfg::SingleLatest);
        });
    }
    builder.configure::<Reading>("ring", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 4 });
    });
    builder.configure::<Reading>("mailbox", |reg| {
        reg.buffer(BufferCfg::Mailbox);
    });
    builder.configure::<Reading>("unbuffered", |_| {});
    let (db, _runner) = builder.build().await.unwrap();
    assert_eq!(
        db.latest::<Reading>("first").unwrap().map(|s| s.value),
        None
    );
    assert_eq!(
        db.latest::<Reading>("unbuffered").unwrap().map(|s| s.value),
        None
    );
    assert!(matches!(
        db.latest::<Reading>("missing"),
        Err(DbError::RecordKeyNotFound { .. })
    ));
    assert!(matches!(
        db.latest::<u32>("first"),
        Err(DbError::TypeMismatch { .. })
    ));
    let mut reader = db.subscribe::<Reading>("first").unwrap();
    db.produce("first", Reading(1)).unwrap();
    db.produce("first", Reading(2)).unwrap();
    db.produce("second", Reading(3)).unwrap();
    for _ in 0..2 {
        assert_eq!(
            db.latest::<Reading>("first").unwrap().map(|s| s.value),
            Some(Reading(2))
        );
    }
    assert_eq!(reader.try_recv().unwrap().value, Reading(2));
    assert_eq!(
        db.latest::<Reading>("first").unwrap().map(|s| s.value),
        Some(Reading(2))
    );
    assert_eq!(
        db.latest::<Reading>("second").unwrap().map(|s| s.value),
        Some(Reading(3))
    );
    db.produce("ring", Reading(4)).unwrap();
    assert_eq!(db.latest::<Reading>("ring").unwrap().map(|s| s.value), None);
    let mut mailbox = db.subscribe::<Reading>("mailbox").unwrap();
    db.produce("mailbox", Reading(5)).unwrap();
    assert_eq!(
        db.latest::<Reading>("mailbox").unwrap().map(|s| s.value),
        Some(Reading(5))
    );
    assert_eq!(
        db.latest::<Reading>("mailbox").unwrap().map(|s| s.value),
        Some(Reading(5))
    );
    assert_eq!(mailbox.try_recv().unwrap().value, Reading(5));
    assert_eq!(
        db.latest::<Reading>("mailbox").unwrap().map(|s| s.value),
        None
    );
}

struct DenyAuth;
impl AuthHandler for DenyAuth {
    fn authenticate<'a>(
        &'a self,
        _: &'a AuthRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Permissions, AuthError>> + Send + 'a>> {
        Box::pin(async { Err(AuthError::new("denied")) })
    }
}

async fn value(
    Extension(db): Extension<AimDb>,
    State(prefix): State<String>,
) -> Result<String, StatusCode> {
    db.latest::<Reading>("value")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|r| format!("{prefix}{}", r.value.0))
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

async fn http_get(addr: std::net::SocketAddr, path: &str) -> String {
    let mut socket = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(socket) = tokio::net::TcpStream::connect(addr).await {
                break socket;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("server did not bind");
    socket
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8(bytes).unwrap()
}

#[tokio::test]
async fn additional_routes_receive_live_db_and_preserve_state_without_ws_auth() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let routes = Router::new()
        .route("/value", get(value))
        .with_state("value=".to_owned());
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(
            WebSocketConnector::new()
                .bind(addr)
                .with_auth(DenyAuth)
                .with_additional_routes(routes),
        );
    builder.configure::<Reading>("value", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });
    let (db, runner) = builder.build().await.unwrap();
    let task = tokio::spawn(runner.run());
    let response = http_get(addr, "/value").await;
    assert!(response.starts_with("HTTP/1.1 503"), "{response}");
    for n in [7, 9] {
        db.produce("value", Reading(n)).unwrap();
        let response = http_get(addr, "/value").await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with(&format!("value={n}")), "{response}");
    }
    let url = format!("ws://{addr}/ws?v={}", aimdb_core::remote::PROTOCOL_VERSION);
    let error = tokio_tungstenite::connect_async(url).await.unwrap_err();
    assert!(
        matches!(error, tokio_tungstenite::tungstenite::Error::Http(ref response) if response.status() == 401)
    );
    assert!(http_get(addr, "/health").await.starts_with("HTTP/1.1 200"));
    assert!(http_get(addr, "/version").await.starts_with("HTTP/1.1 200"));
    task.abort();
    let _ = task.await;
}
