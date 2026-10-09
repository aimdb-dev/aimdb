//! Outbound `ros2://` links against a real Zenoh router, in-process, and the
//! build-time refusals of 053 §4.3.

#![cfg(feature = "std")]

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDbBuilder, DbError};
use aimdb_data_contracts::link_codecs::{self, Json};
use aimdb_data_contracts::{
    LinkCodecBuilderExt, LinkCodecRegistrarExt, Linkable, LinkableRegistrarExt, RosMessage,
    SchemaType, WireFormat,
};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_zenoh_connector::{Ros2Connector, Ros2LinkExt, Ros2Node, ZenohConnector};
use serde::{Deserialize, Serialize};

const HASH: &str = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const DDS: &str = "test_msgs::msg::dds_::Reading_";
const WAIT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "test_msgs/msg/Reading",
    hash = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
)]
struct Reading {
    celsius: f64,
    sensor: String,
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

fn node() -> Ros2Node {
    Ros2Node::new("aimdb_test").namespace("/cell4")
}

fn ros2(endpoint: &str) -> Ros2Connector {
    Ros2Connector::new(endpoint, node())
        .domain_id(7)
        .register::<Reading>()
        .with_zenoh_config(config("client", endpoint, false))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// XXH3-128 of `token`, low half first: rmw_zenoh's GID.
fn gid(token: &str) -> [u8; 16] {
    let h = twox_hash::XxHash3_128::oneshot(token.as_bytes());
    let mut out = [0; 16];
    out[..8].copy_from_slice(&(h as u64).to_le_bytes());
    out[8..].copy_from_slice(&((h >> 64) as u64).to_le_bytes());
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_record_appears_as_a_ros_publisher() {
    let endpoint = format!("tcp/127.0.0.1:{}", free_port());
    let _router = zenoh::open(config("router", &endpoint, true))
        .await
        .unwrap();
    let remote = zenoh::open(config("client", &endpoint, false))
        .await
        .unwrap();
    let data = remote
        .declare_subscriber(format!("7/cell4/reading/{DDS}/{HASH}"))
        .await
        .unwrap();

    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(ros2(&endpoint));
    builder.configure::<Reading>("reading", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 });
        reg.linked_to("ros2://cell4/reading");
        reg.link_to("ros2://cell4/reading_fast")
            .with_link_codec(link_codecs::Default)
            .with_depth(1)
            .finish();
    });
    let (db, runner) = builder.build().await.expect("build");
    tokio::spawn(runner.run());

    let value = Reading {
        celsius: 23.5,
        sensor: "probe".into(),
    };
    let deadline = tokio::time::Instant::now() + WAIT;
    let first = loop {
        db.produce("reading", value.clone()).unwrap();
        if let Ok(Ok(sample)) =
            tokio::time::timeout(Duration::from_millis(200), data.recv_async()).await
        {
            break sample;
        }
        assert!(tokio::time::Instant::now() < deadline, "no ROS sample");
    };
    db.produce("reading", value.clone()).unwrap();
    let second = tokio::time::timeout(WAIT, data.recv_async())
        .await
        .unwrap()
        .unwrap();

    // The payload is the type's CDR.
    let payload = first.payload().to_bytes();
    assert_eq!(Reading::from_bytes(&payload).unwrap(), value);
    assert_eq!(&payload[..4], &[0x00, 0x01, 0x00, 0x00]);

    // The tokens: the node, and one publisher per link, with its QoS.
    let mut tokens = Vec::new();
    let replies = remote.liveliness().get("@ros2_lv/7/**").await.unwrap();
    while let Ok(reply) = replies.recv_async().await {
        if let Ok(sample) = reply.result() {
            tokens.push(sample.key_expr().as_str().to_string());
        }
    }
    let find = |suffix: &str| {
        tokens
            .iter()
            .find(|t| t.ends_with(suffix))
            .unwrap_or_else(|| panic!("no token ending {suffix} in {tokens:#?}"))
            .clone()
    };
    let node_token = find("/0/0/NN/%/%cell4/aimdb_test");
    let zid = node_token.split('/').nth(2).unwrap().to_string();
    let publisher = find(&format!(
        "/MP/%/%cell4/aimdb_test/%cell4%reading/{DDS}/{HASH}/::,10:,:,:,,"
    ));
    let fast = find(&format!(
        "/MP/%/%cell4/aimdb_test/%cell4%reading_fast/{DDS}/{HASH}/::,1:,:,:,,"
    ));
    assert_eq!(
        publisher,
        format!(
            "@ros2_lv/7/{zid}/0/1/MP/%/%cell4/aimdb_test/%cell4%reading/{DDS}/{HASH}/::,10:,:,:,,"
        )
    );
    assert!(
        fast.starts_with(&format!("@ros2_lv/7/{zid}/0/2/MP/")),
        "{fast}"
    );

    // The attachment: sequence from 1, a timestamp, and the publisher's GID.
    let attachment =
        |s: &zenoh::sample::Sample| s.attachment().expect("attachment").to_bytes().to_vec();
    let (a, b) = (attachment(&first), attachment(&second));
    assert_eq!(a.len(), 33);
    let seq = |a: &[u8]| i64::from_le_bytes(a[..8].try_into().unwrap());
    assert_eq!(seq(&b), seq(&a) + 1);
    assert!(i64::from_le_bytes(a[8..16].try_into().unwrap()) > 1_700_000_000_000_000_000);
    assert_eq!(a[16], 0x10);
    assert_eq!(hex(&a[17..]), hex(&gid(&publisher)), "GID of {publisher}");
}

/// Builds a database whose only `ros2://` link is set up by `link`, and
/// returns the build error.
async fn refused(
    connector: Ros2Connector,
    link: impl FnOnce(&mut aimdb_core::RecordRegistrar<'_, Reading>) + Send + 'static,
) -> String {
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(connector);
    builder.configure::<Reading>("reading", move |reg| {
        reg.buffer(BufferCfg::SingleLatest);
        link(reg);
    });
    let error: DbError = builder.build().await.err().expect("refused");
    error.to_string()
}

fn plain(endpoint: &str) -> Ros2Connector {
    Ros2Connector::new(endpoint, node())
        .domain_id(0)
        .register::<Reading>()
}

#[tokio::test]
async fn build_refuses_what_ros_would_not_see() {
    let e = refused(Ros2Connector::new("", node()).domain_id(0), |reg| {
        reg.linked_to("ros2://cell4/reading");
    })
    .await;
    assert!(e.contains("not registered"), "unregistered type: {e}");

    let e = refused(plain(""), |reg| {
        reg.linked_to_with("ros2://cell4/reading", Json);
    })
    .await;
    assert!(
        e.contains("Json") && e.contains("CDR"),
        "non-CDR codec: {e}"
    );

    let e = refused(plain(""), |reg| {
        reg.link_to("ros2://cell4/reading")
            .with_link_codec(link_codecs::Default)
            .with_topic_fn(16, |_v, _out| Ok(false))
            .finish();
    })
    .await;
    assert!(e.contains("topic writer"), "topic writer: {e}");

    let e = refused(plain(""), |reg| {
        reg.linked_to("ros2://cell-4/reading");
    })
    .await;
    assert!(e.contains("'/cell-4/reading'"), "topic name: {e}");

    let e = refused(
        Ros2Connector::new("", Ros2Node::new("aimdb-test"))
            .domain_id(0)
            .register::<Reading>(),
        |reg| {
            reg.linked_to("ros2://reading");
        },
    )
    .await;
    assert!(e.contains("node name 'aimdb-test'"), "node name: {e}");

    let e = refused(
        Ros2Connector::new("", Ros2Node::new("n").namespace("cell4"))
            .domain_id(0)
            .register::<Reading>(),
        |reg| {
            reg.linked_to("ros2://reading");
        },
    )
    .await;
    assert!(e.contains("namespace 'cell4'"), "namespace: {e}");

    let e = refused(plain(""), |reg| {
        reg.linked_from("ros2://cell4/command");
    })
    .await;
    assert!(
        e.contains("inbound ros2:// links are not supported yet"),
        "{e}"
    );
}

/// A hand-written `RosMessage` the derive would never produce.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct HandWritten;

impl SchemaType for HandWritten {
    const NAME: &'static str = "hand_written";
}

impl Linkable for HandWritten {
    const WIRE_FORMAT: WireFormat = WireFormat::Json;
    fn from_bytes(_: &[u8]) -> Result<Self, String> {
        Ok(HandWritten)
    }
    fn to_bytes(&self) -> Result<Vec<u8>, String> {
        Ok(Vec::new())
    }
}

impl RosMessage for HandWritten {
    const ROS_TYPE_NAME: &'static str = "test_msgs::msg::dds_::HandWritten_";
    const ROS_TYPE_HASH: &'static str = "RIHS01_short";
}

#[tokio::test]
async fn build_refuses_a_hand_written_type_that_lies() {
    let connector = Ros2Connector::new("", node())
        .domain_id(0)
        .register::<HandWritten>();
    let e = refused(connector, |reg| {
        reg.linked_to("ros2://reading");
    })
    .await;
    assert!(e.contains("HandWritten") && e.contains("not CDR"), "{e}");
}

#[tokio::test]
async fn without_a_ros2_connector_the_scheme_is_unregistered() {
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(ZenohConnector::new(""));
    builder.configure::<Reading>("reading", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
        reg.linked_to("ros2://cell4/reading");
    });
    let e = builder.build().await.err().expect("refused").to_string();
    assert!(
        e.contains("No connector registered for scheme 'ros2'"),
        "{e}"
    );
}

/// Claims CDR, but its hash is malformed.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct BadHash;

impl SchemaType for BadHash {
    const NAME: &'static str = "bad_hash";
}

impl Linkable for BadHash {
    const WIRE_FORMAT: WireFormat = WireFormat::Cdr;
    fn from_bytes(_: &[u8]) -> Result<Self, String> {
        Ok(BadHash)
    }
    fn to_bytes(&self) -> Result<Vec<u8>, String> {
        Ok(Vec::new())
    }
}

impl RosMessage for BadHash {
    const ROS_TYPE_NAME: &'static str = "test_msgs::msg::dds_::BadHash_";
    const ROS_TYPE_HASH: &'static str = "RIHS01_short";
}

#[tokio::test]
async fn build_refuses_a_malformed_hand_written_hash() {
    let connector = Ros2Connector::new("", node())
        .domain_id(0)
        .register::<BadHash>();
    let e = refused(connector, |reg| {
        reg.linked_to("ros2://reading");
    })
    .await;
    assert!(
        e.contains("BadHash") && e.contains("type hash 'RIHS01_short'"),
        "{e}"
    );
}
