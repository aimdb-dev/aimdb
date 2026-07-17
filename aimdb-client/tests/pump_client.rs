//! `pump_client` mirrors a record **both directions** between a local AimDb and
//! a remote AimDb over the shared session engine.
//!
//! Topology: a server `AimDb` (served by `UdsServer`) and a client `AimDb` whose
//! records carry `uds://` connector links. `run_client` opens the connection;
//! `pump_client` wires the client's outbound/inbound routes to the `ClientHandle`:
//! - **client → server**: producing the client's `cfg` record streams it to the
//!   server via `ClientHandle::write` → the server's `record.set` path.
//! - **server → client**: updating the server's `tele` record streams it back
//!   through a subscription → the client's inbound producer (arbiter path).

// Exercises the UDS transport end-to-end, so it rides that transport feature.
#![cfg(feature = "transport-uds")]

use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::remote::{AimxConfig, SecurityPolicy};
use aimdb_core::session::ClientConfig;
use aimdb_core::{AimDb, AimDbBuilder};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_uds_connector::{UdsClient, UdsServer};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Msg {
    v: u64,
}

/// Re-assert `db.<key>` reaches `want`, re-driving `push` each tick so the test
/// is robust against subscription-registration timing (a fresh subscriber may
/// only see values produced after it attaches).
async fn mirror_reaches(
    db: &Arc<AimDb>,
    key: &str,
    want: &serde_json::Value,
    mut push: impl FnMut(),
) -> bool {
    for _ in 0..100 {
        push();
        tokio::time::sleep(Duration::from_millis(20)).await;
        if db.try_latest_as_json(key).as_ref() == Some(want) {
            return true;
        }
    }
    false
}

#[tokio::test]
async fn pump_client_mirrors_record_both_directions() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("aimdb.sock");

    // --- server: cfg (writable target) + tele (streamed source) ------------
    let mut policy = SecurityPolicy::read_write();
    policy.allow_write_key("cfg");
    let config = AimxConfig::uds_default()
        .socket_path(sock.to_str().unwrap())
        .security_policy(policy);

    let mut sb = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(UdsServer::from_config(config));
    sb.configure::<Msg>("cfg", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    sb.configure::<Msg>("tele", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    let (server_db, server_runner) = sb.build().await.expect("build server db");
    let server_db = Arc::new(server_db);
    // The runner drives both the records and the UDS serve loop.
    tokio::spawn(server_runner.run());

    // --- client: cfg links *to* the server, tele links *from* it -----------
    // UdsClient registers the `uds://` scheme (so the links validate) and, on
    // build, dials the server + drives the mirroring pumps.
    let mut cb = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(UdsClient::new(&sock).with_config(ClientConfig {
            reconnect: true,
            reconnect_delay: 50,
            max_reconnect_delay: 50,
            max_reconnect_attempts: 0,
            keepalive_interval: None,
            max_offline_queue: usize::MAX,
            sends_hello: false,
        }));
    cb.configure::<Msg>("cfg", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .link_to("uds://cfg")
            .with_serializer(|_ctx, m: &Msg| Ok(serde_json::to_vec(m).expect("serialize")))
            .finish();
    });
    cb.configure::<Msg>("tele", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .with_remote_access()
            .link_from("uds://tele")
            .with_deserializer(|_ctx, d: &[u8]| {
                serde_json::from_slice::<Msg>(d).map_err(|e| e.to_string())
            })
            .finish();
    });
    // build() collects the connector's engine + pump futures; the runner drives
    // them (spawn-free engine, driven on a task here).
    let (client_db, client_runner) = cb.build().await.expect("build client db");
    let client_db = Arc::new(client_db);
    tokio::spawn(client_runner.run());

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

    // server → client: updating server `tele` mirrors to client `tele`.
    let want_tele = json!({ "v": 9 });
    let mirrored_in = mirror_reaches(&client_db, "tele", &want_tele, || {
        server_db
            .set_record_from_json("tele", json!({ "v": 9 }))
            .expect("set server tele");
    })
    .await;
    assert!(mirrored_in, "server→client mirror did not reach the client");
}
