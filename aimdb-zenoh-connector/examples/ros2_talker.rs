//! Publishes `std_msgs/msg/String` on `/aimdb_chatter`, as node
//! `/aimdb/talker`, through the Zenoh router at `ZENOH_ENDPOINT`
//! (default `tcp/127.0.0.1:7447`).
//!
//! ```text
//! cargo run -p aimdb-zenoh-connector --features std --example ros2_talker
//! ros2 topic echo /aimdb_chatter        # on a ROS 2 host with rmw_zenoh
//! ```

use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::AimDbBuilder;
use aimdb_data_contracts::{LinkableRegistrarExt, RosMessage};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_zenoh_connector::{Ros2Connector, Ros2Node};
use serde::{Deserialize, Serialize};

/// `std_msgs/msg/String`, with the hash Jazzy and Lyrical publish.
#[derive(Clone, Debug, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "std_msgs/msg/String",
    hash = "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"
)]
struct StringMsg {
    data: String,
}

#[tokio::main]
async fn main() {
    let endpoint = std::env::var("ZENOH_ENDPOINT").unwrap_or_else(|_| "tcp/127.0.0.1:7447".into());
    let ros2 = Ros2Connector::new(endpoint, Ros2Node::new("talker").namespace("/aimdb"))
        .register::<StringMsg>();

    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(ros2);
    builder.configure::<StringMsg>("chatter", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 });
        reg.linked_to("ros2://aimdb_chatter");
    });
    let (db, runner) = builder.build().await.expect("build");
    tokio::spawn(runner.run());

    for n in 1.. {
        db.produce(
            "chatter",
            StringMsg {
                data: format!("aimdb {n}"),
            },
        )
        .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
