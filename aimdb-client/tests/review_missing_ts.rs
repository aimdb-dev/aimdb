use aimdb_client::{AimxConnection, ClientError};
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn review_missing_timestamp_reaches_subscriber_as_protocol_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("review.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (r, mut w) = socket.into_split();
        let mut lines = BufReader::new(r).lines();
        let hello: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let welcome = json!({"t":"reply", "id":hello["id"], "ok":{
            "version":"4.0", "server":"aimdb", "permissions":["read"],
            "writable_records":[], "readonly_records":[]
        }});
        w.write_all(format!("{welcome}\n").as_bytes())
            .await
            .unwrap();
        let sub: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let event =
            json!({"t":"event", "sub":sub["id"].as_u64().unwrap().to_string(), "seq":1, "data":42});
        w.write_all(format!("{event}\n").as_bytes()).await.unwrap();
        std::future::pending::<()>().await;
    });
    let conn = AimxConnection::connect(path.to_str().unwrap())
        .await
        .unwrap();
    let mut stream = conn.subscribe("r").unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_millis(100), stream.next()).await;
    server.abort();
    assert!(
        matches!(result, Ok(Some(Err(ClientError::Protocol { .. })))),
        "missing ts must report Protocol; got {result:?}"
    );
}
