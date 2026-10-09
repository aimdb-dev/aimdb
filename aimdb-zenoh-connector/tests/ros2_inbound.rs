//! Inbound `ros2://` links against a real Zenoh router, in-process, and
//! their build-time refusals.

#![cfg(feature = "std")]

use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDbBuilder, RecordRegistrar};
use aimdb_data_contracts::link_codecs::Json;
use aimdb_data_contracts::{LinkCodecRegistrarExt, Linkable, LinkableRegistrarExt, RosMessage};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_zenoh_connector::{Ros2Connector, Ros2Node};
use serde::{Deserialize, Serialize};

const HASH: &str = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const DDS: &str = "test_msgs::msg::dds_::Command_";
const WAIT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "test_msgs/msg/Command",
    hash = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
)]
struct Command {
    rpm: f64,
    enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "test_msgs/msg/Other",
    hash = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
)]
struct Other {
    value: u32,
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn config(mode: &str, endpoint: &str, listen: bool) -> zenoh::Config {
    let mut config = zenoh::Config::default();
    config
        .insert_json5("mode", &format!(r#""{mode}""#))
        .unwrap();
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .unwrap();
    let field = if listen {
        "listen/endpoints"
    } else {
        "connect/endpoints"
    };
    config
        .insert_json5(field, &format!(r#"["{endpoint}"]"#))
        .unwrap();
    config
}

fn ros2(endpoint: &str) -> Ros2Connector {
    let connector = Ros2Connector::new(endpoint, Ros2Node::new("aimdb_test").namespace("/cell4"))
        .domain_id(7)
        .register::<Command>()
        .register::<Other>();
    // Build-only tests pass no endpoint and never connect.
    if endpoint.is_empty() {
        connector
    } else {
        connector.with_zenoh_config(config("client", endpoint, false))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ros_publisher_reaches_every_record_linked_to_its_topic() {
    let endpoint = format!("tcp/127.0.0.1:{}", free_port());
    let _router = zenoh::open(config("router", &endpoint, true))
        .await
        .unwrap();
    let remote = zenoh::open(config("client", &endpoint, false))
        .await
        .unwrap();

    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(ros2(&endpoint));
    for record in ["spindle.a", "spindle.b"] {
        builder.configure::<Command>(record, |reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 16 });
            reg.linked_from("ros2://cell4/spindle_cmd");
        });
    }
    let (db, runner) = builder.build().await.expect("build");
    let mut a = db.subscribe::<Command>("spindle.a").unwrap();
    let mut b = db.subscribe::<Command>("spindle.b").unwrap();
    tokio::spawn(runner.run());

    // One subscription token per link, after the (absent) publishers.
    let mut tokens = Vec::new();
    let deadline = tokio::time::Instant::now() + WAIT;
    while tokens.len() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "tokens: {tokens:#?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        tokens.clear();
        let replies = remote.liveliness().get("@ros2_lv/7/**").await.unwrap();
        while let Ok(reply) = replies.recv_async().await {
            if let Ok(sample) = reply.result() {
                let token = sample.key_expr().as_str().to_string();
                if token.contains("/MS/") {
                    tokens.push(token);
                }
            }
        }
    }
    tokens.sort();
    for (token, id) in tokens.iter().zip(["1", "2"]) {
        let tail =
            format!("/0/{id}/MS/%/%cell4/aimdb_test/%cell4%spindle_cmd/{DDS}/{HASH}/::,10:,:,:,,");
        assert!(token.ends_with(&tail), "{token}");
    }

    let command = Command {
        rpm: 1200.0,
        enabled: true,
    };
    let key = format!("7/cell4/spindle_cmd/{DDS}/{HASH}");
    remote.delete(key.as_str()).await.unwrap();
    remote
        .put(key.as_str(), command.to_bytes().unwrap())
        .await
        .unwrap();
    for reader in [&mut a, &mut b] {
        let value = tokio::time::timeout(WAIT, reader.recv())
            .await
            .expect("a value")
            .unwrap();
        assert_eq!(value, command, "the delete was not ingested first");
    }
}

/// Builds a database with one inbound `ros2://` link set up by `link`, and
/// returns the build error, if any.
async fn build_with(
    link: impl FnOnce(&mut RecordRegistrar<'_, Command>) + Send + 'static,
    other: bool,
) -> Result<(), String> {
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(ros2(""));
    builder.configure::<Command>("command", move |reg| {
        reg.buffer(BufferCfg::SingleLatest);
        link(reg);
    });
    if other {
        builder.configure::<Other>("other", |reg| {
            reg.buffer(BufferCfg::SingleLatest);
            reg.linked_from("ros2://cell4/spindle_cmd");
        });
    }
    builder.build().await.map(|_| ()).map_err(|e| e.to_string())
}

#[tokio::test]
async fn build_refuses_inbound_links_ros_could_not_serve() {
    let e = build_with(
        |reg| {
            reg.linked_from("ros2://cell4/{cell}/cmd");
        },
        false,
    )
    .await
    .unwrap_err();
    assert!(
        e.contains("does not support topic patterns"),
        "pattern: {e}"
    );

    let e = build_with(
        |reg| {
            reg.linked_from("ros2://cell4/spindle_cmd");
        },
        true,
    )
    .await
    .unwrap_err();
    assert!(e.contains("one type"), "two types: {e}");

    let e = build_with(
        |reg| {
            reg.linked_from_with("ros2://cell4/spindle_cmd", Json);
        },
        false,
    )
    .await
    .unwrap_err();
    assert!(
        e.contains("Json") && e.contains("CDR"),
        "non-CDR codec: {e}"
    );

    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(Ros2Connector::new("", Ros2Node::new("n")).domain_id(0));
    builder.configure::<Command>("command", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
        reg.linked_from("ros2://cell4/spindle_cmd");
    });
    let e = builder.build().await.err().expect("refused").to_string();
    assert!(e.contains("not registered"), "unregistered: {e}");
}

#[tokio::test]
async fn a_custom_deserializer_builds() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let built = build_with(
        move |reg| {
            reg.link_from("ros2://cell4/spindle_cmd")
                .with_deserializer(move |_ctx, bytes: &[u8]| {
                    c.fetch_add(1, Ordering::SeqCst);
                    Command::from_bytes(bytes)
                })
                .finish();
        },
        false,
    )
    .await;
    assert_eq!(built, Ok(()), "the escape hatch warns, it does not refuse");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
