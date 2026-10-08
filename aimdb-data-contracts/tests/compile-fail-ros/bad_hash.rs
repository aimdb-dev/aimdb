use aimdb_data_contracts::RosMessage;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, RosMessage)]
#[ros(type = "std_msgs/msg/String", hash = "RIHS01_ABC")]
struct ShortHash { data: String }

fn main() {}
