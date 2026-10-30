//! `instance.info` and `instance.post_link` (protocol 3.1) through
//! [`AimxConnection`]: against the production UDS server, whose frames are
//! newline-delimited, and against a 3.0 server that does not know the methods.

#![cfg(feature = "transport-uds")]

use std::path::PathBuf;
use std::sync::Arc;

use aimdb_client::protocol::PostLinkReply;
use aimdb_client::{AimxConnection, InstanceInfo};
use aimdb_core::remote::AimxConfig;
use aimdb_core::AimDbBuilder;
use aimdb_tokio_adapter::TokioAdapter;
use aimdb_uds_connector::UdsServer;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

/// A format-1 section around `payload` whose post-link region holds `data`,
/// built by hand and leaked to `'static` as `section!()` is.
fn section(payload: &[u8], data: &[u8]) -> &'static [u8] {
    let region_len = 4 + data.len();
    let region_off = (32 + payload.len()).div_ceil(8) * 8;
    let total = region_off + region_len;
    let mut b = vec![0u8; total];
    b[..8].copy_from_slice(b"AIMDB-BI");
    b[8..10].copy_from_slice(&1u16.to_le_bytes());
    b[10..12].copy_from_slice(&32u16.to_le_bytes());
    b[12..16].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    b[16..20].copy_from_slice(&(region_off as u32).to_le_bytes());
    b[20..24].copy_from_slice(&(region_len as u32).to_le_bytes());
    b[24..28].copy_from_slice(&(total as u32).to_le_bytes());
    b[32..32 + payload.len()].copy_from_slice(payload);
    b[region_off..region_off + 4].copy_from_slice(&(data.len() as u32).to_le_bytes());
    b[region_off + 4..].copy_from_slice(data);
    Box::leak(b.into_boxed_slice())
}

/// Serve `builder` over UDS in `dir`; the socket path.
async fn serve(builder: AimDbBuilder, dir: &tempfile::TempDir) -> String {
    let sock = dir.path().join("aimdb.sock").to_str().unwrap().to_owned();
    let (_db, runner) = builder
        .runtime(Arc::new(TokioAdapter))
        .with_connector(UdsServer::from_config(
            AimxConfig::uds_default().socket_path(&sock),
        ))
        .build()
        .await
        .expect("build db");
    tokio::spawn(runner.run());
    sock
}

#[tokio::test]
async fn instance_info_and_post_link_over_uds() {
    let dir = tempfile::tempdir().unwrap();
    // Whitespace between tokens and an escaped newline in a string: valid on
    // a newline-framed transport, since neither is a raw line break.
    let payload = b"{\"schema\": 1,\t\"name\": \"uds-app\", \"ext\": {\"note\": \"a\\nb\"}}";
    let data: Vec<u8> = (0..1500u32).map(|i| (i * 31) as u8).collect();
    let sock = serve(
        AimDbBuilder::new()
            .build_info(section(payload, &data))
            .instance_name("uds-1")
            .instance_uid(&[0x00, 0x35]),
        &dir,
    )
    .await;

    let conn = AimxConnection::connect(&sock).await.expect("connect");
    assert_eq!(conn.server_info().instance_name.as_deref(), Some("uds-1"));
    // What discovery reports for the instance.
    let found = InstanceInfo::from((PathBuf::from(&sock), conn.server_info().clone()));
    assert_eq!(found.name.as_deref(), Some("uds-1"));

    let info = conn.instance_info().await.unwrap().expect("a 3.1 server");
    assert!(!info.aimdb_version.is_empty());
    assert_eq!(info.instance.name.as_deref(), Some("uds-1"));
    assert_eq!(info.instance.uid.as_deref(), Some("0035"));
    let build = info.build.expect("build info");
    assert_eq!(build.info["ext"]["note"], json!("a\nb"));
    assert_eq!(build.post_link, Some(PostLinkReply::Written { len: 1500 }));

    // Two chunks, stopping at `total`.
    assert_eq!(conn.post_link().await.unwrap(), Some(data));
}

#[tokio::test]
async fn without_build_info() {
    let dir = tempfile::tempdir().unwrap();
    let sock = serve(AimDbBuilder::new(), &dir).await;
    let conn = AimxConnection::connect(&sock).await.expect("connect");
    assert_eq!(conn.server_info().instance_name, None);
    let info = conn.instance_info().await.unwrap().expect("a 3.1 server");
    assert_eq!(info.build, None);
    assert_eq!(info.instance.name, None);
    assert_eq!(conn.post_link().await.unwrap(), None);
}

/// A protocol 3.0 server on the raw AimX wire: it completes the handshake
/// and answers every other request `not_found`.
async fn serve_v30(dir: &tempfile::TempDir) -> String {
    let path = dir.path().join("v30.sock");
    let listener = UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let frame: Value = serde_json::from_str(&line).unwrap();
                    let reply = match (frame["t"].as_str(), frame["method"].as_str()) {
                        (Some("ping"), _) => json!({ "t": "pong" }),
                        (Some("req"), Some("hello")) => json!({
                            "t": "reply", "id": frame["id"], "ok": {
                                "version": "3.0", "server": "aimdb",
                                "permissions": ["read"], "writable_records": [],
                            },
                        }),
                        (Some("req"), _) => {
                            json!({ "t": "reply", "id": frame["id"], "err": "not_found" })
                        }
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
async fn a_3_0_server_reports_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let sock = serve_v30(&dir).await;
    let conn = AimxConnection::connect(&sock)
        .await
        .expect("3.0 is compatible");
    assert_eq!(conn.server_info().version, "3.0");
    assert_eq!(conn.server_info().instance_name, None);
    assert_eq!(conn.instance_info().await.unwrap(), None);
    assert_eq!(conn.post_link().await.unwrap(), None);
}
