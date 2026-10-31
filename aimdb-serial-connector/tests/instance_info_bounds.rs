//! `instance.info` and `instance.post_link` stay within serial frames, the
//! smallest AimX frames: the largest identity `build()` accepts (4 KiB
//! payload, a 128-byte name of control characters, a 32-byte UID) and a full
//! 4 KiB post-link region, served over COBS through a small pipe. The
//! receiving side drops any frame over `DEFAULT_MAX_FRAME`, so every reply
//! that arrives fits.

#![cfg(feature = "std")]

use std::sync::Arc;
use std::time::Duration;

use aimdb_core::remote::{
    AimxConfig, InstanceInfoReply, PostLinkChunk, INSTANCE_INFO_REPLY_MAX, POST_LINK_CHUNK_MAX,
};
use aimdb_core::session::aimx::{AimxCodec, AimxDispatch};
use aimdb_core::session::{
    run_client, serve, ClientConfig, ClientHandle, Dispatch, EnvelopeCodec, Outbound, Payload,
    SessionConfig, SessionLimits,
};
use aimdb_core::session::{OneShotDialer, OneShotListener};
use aimdb_core::AimDbBuilder;
use aimdb_serial_connector::connector::framed;
use aimdb_serial_connector::framing::{encode_frame, DEFAULT_MAX_FRAME};
use aimdb_tokio_adapter::net::TokioByteStream;
use aimdb_tokio_adapter::TokioAdapter;
use serde_json::json;

const REGION_LEN: usize = 4096;

/// The largest payload the build helper writes: a compact object of exactly
/// `PAYLOAD_MAX` bytes.
fn max_payload() -> Vec<u8> {
    let frame = br#"{"schema":1,"ext":{"pad":""}}"#.len();
    let pad = "x".repeat(aimdb_build_info::PAYLOAD_MAX - frame);
    let payload = serde_json::to_vec(&json!({ "schema": 1, "ext": { "pad": pad } })).unwrap();
    assert_eq!(payload.len(), aimdb_build_info::PAYLOAD_MAX);
    payload
}

/// The section image the build helper writes, with its region written after
/// "linking" to hold `data`.
fn section(payload: &[u8], data: &[u8]) -> &'static [u8] {
    let mut image = aimdb_build_info::encode(payload, REGION_LEN).unwrap();
    let region = u32::from_le_bytes(image[16..20].try_into().unwrap()) as usize;
    image[region..region + 4].copy_from_slice(&(data.len() as u32).to_le_bytes());
    image[region + 4..region + 4 + data.len()].copy_from_slice(data);
    Box::leak(image.into_boxed_slice())
}

/// Bytes of the serial frame that carries `reply`: the AimX reply envelope
/// with the longest request id, COBS-encoded and delimited.
fn framed_len(reply: &[u8]) -> usize {
    let mut envelope = Vec::new();
    AimxCodec
        .encode(
            Outbound::Reply {
                id: u64::MAX,
                result: Ok(Payload::from(reply)),
            },
            &mut envelope,
        )
        .unwrap();
    let mut frame = Vec::new();
    encode_frame(&envelope, &mut frame);
    frame.len()
}

async fn call(handle: &ClientHandle, method: &str, params: serde_json::Value) -> Vec<u8> {
    let params = Payload::from(serde_json::to_vec(&params).unwrap().as_slice());
    tokio::time::timeout(Duration::from_secs(10), handle.call(method, params))
        .await
        .expect("a reply within the timeout: an oversized frame is dropped")
        .expect("rpc ok")
        .to_vec()
}

#[tokio::test]
async fn worst_case_identity_fits_serial_frames() {
    let payload = max_payload();
    let data: Vec<u8> = (0..(REGION_LEN - 4) as u32)
        .map(|i| (i * 7) as u8)
        .collect();
    let name: String = std::iter::repeat_n('\u{1}', aimdb_core::identity::NAME_MAX).collect();
    let (db, runner) = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .build_info(section(&payload, &data))
        .instance_name(name.clone())
        .instance_uid(&[0xff; aimdb_core::identity::UID_MAX])
        .build()
        .await
        .expect("the largest identity build() accepts");
    let db = Arc::new(db);
    tokio::spawn(runner.run());

    // A small pipe: every frame crosses in many short reads, as on a UART.
    let (server_end, client_end) = tokio::io::duplex(64);
    let dispatch: Arc<dyn Dispatch> =
        Arc::new(AimxDispatch::new(db.clone(), AimxConfig::uds_default()));
    tokio::spawn(serve(
        OneShotListener::new(framed(TokioByteStream(server_end))),
        Arc::new(AimxCodec),
        dispatch,
        SessionConfig {
            limits: SessionLimits {
                max_connections: 1,
                max_subs_per_connection: 8,
            },
            reads_hello: false,
            acks_subscribe: false,
        },
    ));
    let (handle, engine) = run_client(
        OneShotDialer::new(framed(TokioByteStream(client_end))),
        AimxCodec,
        ClientConfig {
            sends_hello: false,
            ..ClientConfig::default()
        },
        Arc::new(TokioAdapter),
    );
    tokio::spawn(engine);

    // The welcome carries the escaped name.
    let welcome = call(&handle, "hello", json!({ "version": "3.1" })).await;
    assert!(framed_len(&welcome) <= DEFAULT_MAX_FRAME);

    let reply = call(&handle, "instance.info", json!(null)).await;
    assert!(
        reply.len() <= INSTANCE_INFO_REPLY_MAX,
        "instance.info is {} bytes",
        reply.len()
    );
    assert!(
        framed_len(&reply) <= DEFAULT_MAX_FRAME,
        "framed instance.info is {} bytes",
        framed_len(&reply)
    );
    let info: InstanceInfoReply = serde_json::from_slice(&reply).unwrap();
    assert_eq!(info.instance.name.as_deref(), Some(name.as_str()));
    assert_eq!(
        info.build.unwrap().info,
        serde_json::from_slice::<serde_json::Value>(&payload).unwrap()
    );

    // The whole region, one full chunk at a time.
    let mut read = Vec::new();
    while read.len() < data.len() {
        let reply = call(
            &handle,
            "instance.post_link",
            json!({ "offset": read.len(), "len": POST_LINK_CHUNK_MAX }),
        )
        .await;
        assert!(framed_len(&reply) <= DEFAULT_MAX_FRAME);
        let chunk: PostLinkChunk = serde_json::from_slice(&reply).unwrap();
        assert_eq!(chunk.total as usize, data.len());
        assert!(!chunk.data.is_empty());
        let bytes: Vec<u8> = (0..chunk.data.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&chunk.data[i..i + 2], 16).unwrap())
            .collect();
        read.extend_from_slice(&bytes);
    }
    assert_eq!(read, data);
}
