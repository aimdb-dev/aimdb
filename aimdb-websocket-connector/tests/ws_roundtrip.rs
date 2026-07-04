//! Layer 1.3 (doc 039-validation) — full **AimDB ↔ AimDB over a real WebSocket**.
//!
//! A server `AimDb` (served by `WebSocketConnector`) and a client `AimDb` (whose
//! records carry `ws-client://` links via `WsClientConnector`). This exercises the
//! whole public path both directions over a real socket:
//! - **client → server**: producing the client's `cfg` record writes it to the
//!   server (`link_to("ws-client://cfg")` → `Write` → server `link_from("ws://cfg")`).
//! - **server → client**: updating the server's `tele` record streams it back
//!   (`link_to("ws://tele")` broadcast → client subscription → client `tele`).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDb, AimDbBuilder};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_websocket_connector::{WebSocketConnector, WsClientConnector};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Msg {
    v: u64,
}

/// Grab a probably-free ephemeral port (the WS builder binds internally and does
/// not surface `:0`'s assigned port, so we pick one up front).
fn free_addr() -> SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let a = l.local_addr().unwrap();
    drop(l);
    a
}

/// Re-assert `db.<key>` reaches `want`, re-driving `push` each tick so the test is
/// robust against subscription-registration timing.
async fn mirror_reaches(
    db: &Arc<AimDb>,
    key: &str,
    want: &serde_json::Value,
    mut push: impl FnMut(),
) -> bool {
    for _ in 0..100 {
        push();
        tokio::time::sleep(Duration::from_millis(25)).await;
        if db.try_latest_as_json(key).as_ref() == Some(want) {
            return true;
        }
    }
    false
}

#[tokio::test]
async fn ws_mirrors_record_both_directions() {
    let addr = free_addr();

    // --- server: tele (broadcast source) + cfg (inbound write target) --------
    let mut sb = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(WebSocketConnector::new().bind(addr).path("/ws"));
    sb.configure::<Msg>("tele", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .link_to("ws://tele")
            .with_serializer(|_ctx, m: &Msg| Ok(serde_json::to_vec(m).expect("serialize")))
            .finish();
    });
    sb.configure::<Msg>("cfg", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .link_from("ws://cfg")
            .with_deserializer(|_ctx, d: &[u8]| {
                serde_json::from_slice::<Msg>(d).map_err(|e| e.to_string())
            })
            .finish();
    });
    let (server_db, server_runner) = sb.build().await.expect("build server db");
    let server_db = Arc::new(server_db);
    tokio::spawn(server_runner.run());

    // Give the server a moment to bind before the client dials.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // --- client: cfg links *to* the server, tele links *from* it -------------
    let mut cb = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(WsClientConnector::new(format!("ws://{addr}/ws")));
    cb.configure::<Msg>("cfg", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .link_to("ws-client://cfg")
            .with_serializer(|_ctx, m: &Msg| Ok(serde_json::to_vec(m).expect("serialize")))
            .finish();
    });
    cb.configure::<Msg>("tele", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .link_from("ws-client://tele")
            .with_deserializer(|_ctx, d: &[u8]| {
                serde_json::from_slice::<Msg>(d).map_err(|e| e.to_string())
            })
            .finish();
    });
    let (client_db, client_runner) = cb.build().await.expect("build client db");
    let client_db = Arc::new(client_db);
    tokio::spawn(client_runner.run());

    // server → client: updating server `tele` mirrors to client `tele`.
    let want_tele = json!({ "v": 9 });
    let mirrored_in = mirror_reaches(&client_db, "tele", &want_tele, || {
        server_db
            .set_record_from_json("tele", json!({ "v": 9 }))
            .expect("set server tele");
    })
    .await;
    assert!(mirrored_in, "server→client mirror did not reach the client");

    // client → server: producing client `cfg` mirrors to server `cfg`.
    let want_cfg = json!({ "v": 7 });
    let mirrored_out = mirror_reaches(&server_db, "cfg", &want_cfg, || {
        client_db
            .set_record_from_json("cfg", json!({ "v": 7 }))
            .expect("set client cfg");
    })
    .await;
    assert!(
        mirrored_out,
        "client→server mirror did not reach the server"
    );
}
