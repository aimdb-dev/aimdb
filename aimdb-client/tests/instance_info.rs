//! `instance.info` and `instance.post_link` (protocol 3.1) through
//! [`AimxConnection`]: against the production UDS server, whose frames are
//! newline-delimited, and that a 3.x server is refused at connect.

#![cfg(feature = "transport-uds")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
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

    let info = conn.instance_info().await.unwrap().expect("a 4.0 server");
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
    let info = conn.instance_info().await.unwrap().expect("a 4.0 server");
    assert_eq!(info.build, None);
    assert_eq!(info.instance.name, None);
    assert_eq!(conn.post_link().await.unwrap(), None);
}

/// What a fake server answers to one `instance.post_link` request: the
/// reply body, or `None` for `not_found`.
type PostLinkFn = dyn Fn(&Value) -> Option<Value> + Send + Sync;

/// A server on the raw AimX wire: it completes the handshake with protocol
/// `version`, answers `instance.post_link` from `post_link`, and every other
/// request `not_found`. Returns the socket path and the number of
/// `instance.post_link` requests it has seen.
async fn serve_fake(
    dir: &tempfile::TempDir,
    version: &'static str,
    post_link: Box<PostLinkFn>,
) -> (String, Arc<AtomicUsize>) {
    let path = dir.path().join("fake.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let post_link: Arc<PostLinkFn> = Arc::from(post_link);
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (post_link, seen) = (post_link.clone(), seen.clone());
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let frame: Value = serde_json::from_str(&line).unwrap();
                    let ok = |body: Value| json!({ "t": "reply", "id": frame["id"], "ok": body });
                    let not_found = json!({ "t": "reply", "id": frame["id"], "err": "not_found" });
                    let reply = match (frame["t"].as_str(), frame["method"].as_str()) {
                        (Some("ping"), _) => json!({ "t": "pong" }),
                        (Some("req"), Some("hello")) => ok(json!({
                            "version": version, "server": "aimdb",
                            "permissions": ["read"], "writable_records": [],
                        })),
                        (Some("req"), Some("instance.post_link")) => {
                            seen.fetch_add(1, Ordering::SeqCst);
                            post_link(&frame["params"]).map_or(not_found, ok)
                        }
                        (Some("req"), _) => not_found,
                        _ => continue,
                    };
                    let mut bytes = serde_json::to_vec(&reply).unwrap();
                    bytes.push(b'\n');
                    write.write_all(&bytes).await.unwrap();
                }
            });
        }
    });
    (path.to_str().unwrap().to_owned(), requests)
}

/// A protocol 3.x server: it sends values without write stamps.
async fn serve_v3(dir: &tempfile::TempDir) -> String {
    serve_fake(dir, "3.1", Box::new(|_| None)).await.0
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// An honest chunk of `data` for the request's `offset` and `len`.
fn honest(data: &[u8], params: &Value) -> Value {
    let offset = params["offset"].as_u64().unwrap() as usize;
    let len = params["len"].as_u64().unwrap() as usize;
    let end = (offset + len).min(data.len());
    json!({ "offset": offset, "total": data.len(), "data": hex(&data[offset.min(end)..end]) })
}

#[tokio::test]
async fn post_link_reads_every_chunk_of_a_consistent_server() {
    let dir = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..3000u32).map(|i| (i * 7) as u8).collect();
    let served = data.clone();
    let (sock, requests) =
        serve_fake(&dir, "4.0", Box::new(move |p| Some(honest(&served, p)))).await;
    let conn = AimxConnection::connect(&sock).await.expect("connect");
    assert_eq!(conn.post_link().await.unwrap(), Some(data));
    assert_eq!(
        requests.load(Ordering::SeqCst),
        3,
        "1024 + 1024 + 952 bytes"
    );
}

/// A malformed peer gets an error, never a truncated or wrong result.
#[tokio::test]
async fn post_link_rejects_inconsistent_chunks() {
    let data: Vec<u8> = (0..1500u32).map(|i| i as u8).collect();
    let cases: Vec<(&str, Box<PostLinkFn>, usize)> = vec![
        (
            "a total no section can hold, refused before reading on",
            Box::new(|_| Some(json!({ "offset": 0, "total": u32::MAX, "data": "00" }))),
            1,
        ),
        (
            "every chunk claims offset 0",
            {
                let data = data.clone();
                Box::new(move |p| {
                    let mut c = honest(&data, p);
                    c["offset"] = json!(0);
                    Some(c)
                })
            },
            2,
        ),
        (
            "the total changes between chunks",
            {
                let data = data.clone();
                Box::new(move |p| {
                    let mut c = honest(&data, p);
                    if p["offset"] != 0 {
                        c["total"] = json!(2000);
                    }
                    Some(c)
                })
            },
            2,
        ),
        (
            "the last chunk runs past the total",
            {
                let data = data.clone();
                Box::new(move |p| {
                    let mut c = honest(&data, p);
                    if p["offset"] != 0 {
                        c["data"] = json!(hex(&[0u8; 1024]));
                    }
                    Some(c)
                })
            },
            2,
        ),
    ];
    for (case, post_link, expected_requests) in cases {
        let dir = tempfile::tempdir().unwrap();
        let (sock, requests) = serve_fake(&dir, "4.0", post_link).await;
        let conn = AimxConnection::connect(&sock).await.expect("connect");
        let err = conn.post_link().await.expect_err(case);
        assert!(
            err.to_string()
                .contains("malformed instance.post_link reply"),
            "{case}: {err}"
        );
        assert_eq!(requests.load(Ordering::SeqCst), expected_requests, "{case}");
    }
}

/// AimX 4.0 makes `ts` mandatory, so a 3.x server is refused at connect
/// instead of failing later on its unstamped replies.
#[tokio::test]
async fn a_3_x_server_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let sock = serve_v3(&dir).await;
    let Err(err) = AimxConnection::connect(&sock).await else {
        panic!("a 3.x server must be refused");
    };
    assert!(
        err.to_string()
            .contains("protocol version mismatch: server speaks 3.1"),
        "{err}"
    );
}
