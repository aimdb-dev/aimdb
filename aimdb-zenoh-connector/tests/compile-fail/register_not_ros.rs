use aimdb_zenoh_connector::{Ros2Connector, Ros2Node};

struct NotRos;

fn main() {
    let _ = Ros2Connector::new("tcp/127.0.0.1:7447", Ros2Node::new("n")).register::<NotRos>();
}
