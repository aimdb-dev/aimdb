use aimdb_data_contracts::RosMessage;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, RosMessage)]
#[ros(type = "std_msgs/msg/String", hash = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff", depth = 10)]
struct Unknown { data: String }

fn main() {}
