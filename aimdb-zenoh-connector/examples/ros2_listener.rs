//! Prints every `std_msgs/msg/String` published on `/aimdb_cmd`, as node
//! `/aimdb/listener`, through the Zenoh router at `ZENOH_ENDPOINT`
//! (default `tcp/127.0.0.1:7447`).
//!
//! ```text
//! cargo run -p aimdb-zenoh-connector --features std --example ros2_listener
//! ros2 topic pub /aimdb_cmd std_msgs/msg/String "{data: hello}"
//! ```

use std::sync::Arc;

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
    let ros2 = Ros2Connector::new(endpoint, Ros2Node::new("listener").namespace("/aimdb"))
        .register::<StringMsg>();

    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(ros2);
    builder.configure::<StringMsg>("command", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 });
        reg.linked_from("ros2://aimdb_cmd");
    });
    let (db, runner) = builder.build().await.expect("build");
    let mut reader = db.subscribe::<StringMsg>("command").expect("reader");
    tokio::spawn(runner.run());

    while let Ok(msg) = reader.recv().await {
        println!("received: {}", msg.data);
    }
}
