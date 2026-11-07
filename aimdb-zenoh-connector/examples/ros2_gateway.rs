//! A ROS 2 → Zenoh telemetry gateway: every `geometry_msgs/msg/Twist` on
//! `/cmd_vel` becomes a typed AimDB record, is republished as JSON on the
//! plain Zenoh key `telemetry/cmd_vel`, and can be watched over AimX.
//!
//! `ros2://` and `zenoh://` share one session to the router at
//! `ZENOH_ENDPOINT` (default `tcp/127.0.0.1:7447`). The ROS domain comes from
//! `ROS_DOMAIN_ID` (default 0). AimX listens on `AIMX_SOCKET` (default
//! `/tmp/aimdb-ros2-gateway.sock`), read-only.
//!
//! ```text
//! cargo run -p aimdb-zenoh-connector --features std --example ros2_gateway
//! ros2 topic pub /cmd_vel geometry_msgs/msg/Twist \
//!     "{linear: {x: 0.5}, angular: {z: 0.3}}"
//! aimdb watch robot.cmd_vel
//! cargo run -p aimdb-zenoh-connector --features std --example zenoh_sub -- 'telemetry/**'
//! ```

use std::sync::Arc;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::remote::{AimxConfig, SecurityPolicy};
use aimdb_core::AimDbBuilder;
use aimdb_data_contracts::link_codecs::Json;
use aimdb_data_contracts::{LinkCodecRegistrarExt, LinkableRegistrarExt, RosMessage};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_uds_connector::UdsServer;
use aimdb_zenoh_connector::{Ros2Node, ZenohConnector};
use serde::{Deserialize, Serialize};

/// `geometry_msgs/msg/Vector3`: `float64 x, y, z`.
#[derive(Clone, Debug, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "geometry_msgs/msg/Vector3",
    hash = "RIHS01_cc12fe83e4c02719f1ce8070bfd14aecd40f75a96696a67a2a1f37f7dbb0765d"
)]
struct Vector3 {
    x: f64,
    y: f64,
    z: f64,
}

/// `geometry_msgs/msg/Twist`, with the hash Jazzy and Lyrical publish
/// (`ros2 topic info -v /cmd_vel`). The fields are in `.msg` order: CDR is
/// positional, and `Vector3` is encoded inline.
#[derive(Clone, Debug, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "geometry_msgs/msg/Twist",
    hash = "RIHS01_9c45bf16fe0983d80e3cfe750d6835843d265a9a6c46bd2e609fcddde6fb8d2a"
)]
struct Twist {
    linear: Vector3,
    angular: Vector3,
}

#[tokio::main]
async fn main() {
    let endpoint = std::env::var("ZENOH_ENDPOINT").unwrap_or_else(|_| "tcp/127.0.0.1:7447".into());
    let socket =
        std::env::var("AIMX_SOCKET").unwrap_or_else(|_| "/tmp/aimdb-ros2-gateway.sock".into());

    // One Zenoh session: the ROS view is derived from the plain connector.
    let zenoh = ZenohConnector::new(endpoint.as_str());
    let ros2 = zenoh
        .ros2(Ros2Node::new("telemetry_gateway").namespace("/aimdb"))
        .register::<Twist>();

    // AimX over a Unix socket, for `aimdb watch`. Read-only: ROS is the only
    // writer of the record.
    let _ = std::fs::remove_file(&socket);
    let aimx = UdsServer::from_config(
        AimxConfig::uds_default()
            .socket_path(socket.as_str())
            .security_policy(SecurityPolicy::read_only()),
    );

    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(zenoh)
        .with_connector(ros2)
        .with_connector(aimx);
    builder.configure::<Twist>("robot.cmd_vel", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .with_remote_access()
            .linked_from("ros2://cmd_vel") // CDR, as a ROS subscription
            .linked_to_with("zenoh://telemetry/cmd_vel", Json); // JSON, plain Zenoh
    });
    let (db, runner) = builder.build().await.expect("build");
    let mut reader = db.subscribe::<Twist>("robot.cmd_vel").expect("reader");
    tokio::spawn(runner.run());

    println!("ros2_gateway: /cmd_vel -> robot.cmd_vel -> telemetry/cmd_vel");
    println!("ros2_gateway: router {endpoint}, AimX unix://{socket}");
    while let Ok(twist) = reader.recv().await {
        let Twist { linear, angular } = &twist.value;
        println!(
            "cmd_vel: linear ({}, {}, {}) angular ({}, {}, {})",
            linear.x, linear.y, linear.z, angular.x, angular.y, angular.z
        );
    }
}
