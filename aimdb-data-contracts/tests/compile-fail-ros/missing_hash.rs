use aimdb_data_contracts::RosMessage;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, RosMessage)]
#[ros(type = "std_msgs/msg/String")]
struct NoHash { data: String }

fn main() {}
