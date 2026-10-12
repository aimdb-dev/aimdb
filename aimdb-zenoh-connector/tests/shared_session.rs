//! A `ZenohConnector` and the `Ros2Connector` derived from it share one
//! session: one TCP connection to the router, whichever is registered first.

#![cfg(feature = "std")]

use std::net::TcpListener as StdListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::AimDbBuilder;
use aimdb_data_contracts::{Linkable, LinkableRegistrarExt, RosMessage};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_zenoh_connector::{Ros2Node, ZenohConnector};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};

const HASH: &str = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const DDS: &str = "test_msgs::msg::dds_::Temperature_";
const WAIT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "test_msgs/msg/Temperature",
    hash = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
)]
struct Temperature {
    celsius: f64,
}

fn free_port() -> u16 {
    StdListener::bind("127.0.0.1:0")
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

/// Forwards `127.0.0.1:<returned port>` to `router_port`, counting the
/// connections made through it.
async fn counting_proxy(router_port: u16) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    tokio::spawn(async move {
        while let Ok((mut inbound, _)) = listener.accept().await {
            c.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                if let Ok(mut outbound) = TcpStream::connect(("127.0.0.1", router_port)).await {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
            });
        }
    });
    (port, count)
}

/// The gateway (053 §4.8): `zenoh://` in, `ros2://` out, on one session.
async fn gateway(zenoh_first: bool) {
    let router_port = free_port();
    let router = format!("tcp/127.0.0.1:{router_port}");
    let _router = zenoh::open(config("router", &router, true)).await.unwrap();
    let remote = zenoh::open(config("client", &router, false)).await.unwrap();
    let ros = remote
        .declare_subscriber(format!("7/cell4/temperature/{DDS}/{HASH}"))
        .await
        .unwrap();

    let (proxy_port, connections) = counting_proxy(router_port).await;
    let through_proxy = format!("tcp/127.0.0.1:{proxy_port}");
    let zenoh = ZenohConnector::new(through_proxy.as_str()).with_zenoh_config(config(
        "client",
        &through_proxy,
        false,
    ));
    let ros2 = zenoh
        .ros2(Ros2Node::new("cell4_gateway"))
        .domain_id(7)
        .register::<Temperature>();

    let builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter::new().unwrap()));
    let mut builder = if zenoh_first {
        builder.with_connector(zenoh).with_connector(ros2)
    } else {
        builder.with_connector(ros2).with_connector(zenoh)
    };
    builder.configure::<Temperature>("cell4.temperature", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .linked_from("zenoh://cell4/probe/temperature")
            .linked_to("ros2://cell4/temperature");
    });
    let (_db, runner) = builder.build().await.expect("build");
    tokio::spawn(runner.run());

    // An MCU's plain Zenoh publish comes out as a ROS sample.
    let reading = Temperature { celsius: 23.5 };
    let deadline = tokio::time::Instant::now() + WAIT;
    let sample = loop {
        remote
            .put("cell4/probe/temperature", reading.to_bytes().unwrap())
            .await
            .unwrap();
        if let Ok(Ok(sample)) =
            tokio::time::timeout(Duration::from_millis(200), ros.recv_async()).await
        {
            break sample;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "nothing reached ROS"
        );
    };
    assert_eq!(
        Temperature::from_bytes(&sample.payload().to_bytes()).unwrap(),
        reading
    );
    assert_eq!(sample.attachment().expect("attachment").len(), 33);

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "one router connection"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_gateway_opens_one_connection_with_zenoh_registered_first() {
    gateway(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_gateway_opens_one_connection_with_ros2_registered_first() {
    gateway(false).await;
}

/// A `.ros2(..)` view that is never registered leaves the Zenoh side alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unregistered_ros2_view_does_not_stop_the_zenoh_connector() {
    let endpoint = format!("tcp/127.0.0.1:{}", free_port());
    let _router = zenoh::open(config("router", &endpoint, true))
        .await
        .unwrap();
    let remote = zenoh::open(config("client", &endpoint, false))
        .await
        .unwrap();
    let out = remote.declare_subscriber("cell4/state").await.unwrap();

    let zenoh = ZenohConnector::new(endpoint.as_str())
        .with_zenoh_config(config("client", &endpoint, false));
    let _unused = zenoh.ros2(Ros2Node::new("never_registered"));
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(zenoh);
    builder.configure::<Temperature>("state", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .linked_to("zenoh://cell4/state");
    });
    let (db, runner) = builder.build().await.expect("build");
    tokio::spawn(runner.run());

    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        db.produce("state", Temperature { celsius: 1.0 }).unwrap();
        if let Ok(Ok(_)) = tokio::time::timeout(Duration::from_millis(200), out.recv_async()).await
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "no zenoh sample");
    }
}

/// One ROS node per session: two views both claim `ros2://`.
#[tokio::test]
async fn two_ros2_views_on_one_session_are_refused() {
    let zenoh = ZenohConnector::new("");
    let a = zenoh.ros2(Ros2Node::new("a")).domain_id(0);
    let b = zenoh.ros2(Ros2Node::new("b")).domain_id(0);
    let builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(zenoh)
        .with_connector(a)
        .with_connector(b);
    let error = builder.build().await.err().expect("refused").to_string();
    assert!(error.contains("ros2"), "{error}");
}

/// The control: two connectors that do not share a session open two
/// connections through the same proxy.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn separate_connectors_open_two_connections() {
    let router_port = free_port();
    let router = format!("tcp/127.0.0.1:{router_port}");
    let _router = zenoh::open(config("router", &router, true)).await.unwrap();
    let (proxy_port, connections) = counting_proxy(router_port).await;
    let through_proxy = format!("tcp/127.0.0.1:{proxy_port}");

    let zenoh = ZenohConnector::new(through_proxy.as_str()).with_zenoh_config(config(
        "client",
        &through_proxy,
        false,
    ));
    let ros2 = aimdb_zenoh_connector::Ros2Connector::new(
        through_proxy.as_str(),
        Ros2Node::new("separate"),
    )
    .domain_id(7)
    .register::<Temperature>()
    .with_zenoh_config(config("client", &through_proxy, false));
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(zenoh)
        .with_connector(ros2);
    builder.configure::<Temperature>("t", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .linked_from("zenoh://cell4/probe/temperature")
            .linked_to("ros2://cell4/temperature");
    });
    let (_db, runner) = builder.build().await.expect("build");
    tokio::spawn(runner.run());

    let deadline = tokio::time::Instant::now() + WAIT;
    while connections.load(Ordering::SeqCst) < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "connections: {}",
            connections.load(Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(connections.load(Ordering::SeqCst), 2);
}
