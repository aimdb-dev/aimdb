//! A record's history is stored and authorized under its record key (#426).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::AimDbBuilder;
use aimdb_persistence::{
    AimDbBuilderPersistExt, BoxFuture, PersistenceBackend, PersistenceError, QueryParams,
    RecordRegistrarPersistExt, StoredValue,
};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_websocket_connector::{
    AuthError, AuthHandler, AuthRequest, Permissions, WebSocketConnector,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

#[derive(Default)]
struct MemBackend(Mutex<Vec<StoredValue>>);

impl PersistenceBackend for MemBackend {
    fn store<'a>(
        &'a self,
        record_name: &'a str,
        value: &'a Value,
        stored_at: u64,
    ) -> BoxFuture<'a, Result<(), PersistenceError>> {
        self.0.lock().unwrap().push(StoredValue {
            record_name: record_name.into(),
            value: value.clone(),
            stored_at,
        });
        Box::pin(async { Ok(()) })
    }

    fn query<'a>(
        &'a self,
        pattern: &'a str,
        _params: QueryParams,
    ) -> BoxFuture<'a, Result<Vec<StoredValue>, PersistenceError>> {
        let rows = self.0.lock().unwrap().clone();
        let rows = rows
            .into_iter()
            .filter(|r| aimdb_core::topic_matches(pattern, &r.record_name))
            .collect();
        Box::pin(async move { Ok(rows) })
    }

    fn cleanup(&self, _older_than: u64) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        Box::pin(async { Ok(0) })
    }
}

/// `?grant=<pattern>` becomes the client's only read pattern.
struct GrantFromQuery;
impl AuthHandler for GrantFromQuery {
    fn authenticate<'a>(
        &'a self,
        request: &'a AuthRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Permissions, AuthError>> + Send + 'a>> {
        let grant = request.query_params["grant"].clone();
        Box::pin(async move {
            Ok(Permissions {
                read_patterns: vec![grant],
                write_patterns: Vec::new(),
            })
        })
    }
}

/// Connects with `grant`, sends `record.query {"name": "#"}`, returns `ok`.
async fn query_history(addr: std::net::SocketAddr, grant: &str) -> Value {
    let url = aimdb_core::remote::ws_url_with_version(&format!("ws://{addr}/ws"));
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("{url}&grant={grant}"))
        .await
        .expect("connect");
    let req = json!({"t": "req", "id": 1, "method": "record.query", "params": {"name": "#"}});
    ws.send(Message::Text(req.to_string().into()))
        .await
        .unwrap();
    loop {
        let Some(Ok(Message::Text(t))) = ws.next().await else {
            panic!("connection closed before reply");
        };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["t"] == "reply" {
            return v["ok"].clone();
        }
    }
}

#[tokio::test]
async fn history_is_stored_and_authorized_under_record_key() {
    let backend = Arc::new(MemBackend::default());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(
            WebSocketConnector::new()
                .with_auth(GrantFromQuery)
                .bind(addr)
                .path("/ws"),
        )
        .with_persistence(backend.clone(), Duration::from_secs(3600));
    builder.configure::<f64>("temp.vienna", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .persist();
    });
    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());

    db.set_record_from_json("temp.vienna", json!(21.5)).unwrap();
    while backend.0.lock().unwrap().is_empty() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(backend.0.lock().unwrap()[0].record_name, "temp.vienna");

    let history = query_history(addr, "temp.*").await;
    assert_eq!(history["records"][0]["topic"], "temp.vienna");
    assert_eq!(history["total"], 1);

    let history = query_history(addr, "other.*").await;
    assert_eq!(history, json!({"records": [], "total": 0}));
}
