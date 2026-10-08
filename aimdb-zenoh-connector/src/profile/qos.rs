//! The QoS field of a liveliness token.

use alloc::string::String;

/// rmw_zenoh's own default depth: a token leaves the depth empty for it.
const RMW_ZENOH_DEFAULT_DEPTH: u32 = 42;

/// The QoS a `ros2://` link advertises. Deadline, lifespan and liveliness
/// always take the ROS defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Qos {
    pub reliability: Reliability,
    pub durability: Durability,
    pub history: History,
    pub depth: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reliability {
    Reliable,
    BestEffort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Durability {
    Volatile,
    TransientLocal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum History {
    KeepLast,
    KeepAll,
}

impl Default for Qos {
    /// The rmw default profile, as a default-constructed `rclcpp` publisher
    /// has it: `KEEP_LAST` 10, `RELIABLE`, `VOLATILE`.
    fn default() -> Self {
        Self {
            reliability: Reliability::Reliable,
            durability: Durability::Volatile,
            history: History::KeepLast,
            depth: 10,
        }
    }
}

impl Qos {
    /// `<reliability>:<durability>:<history>,<depth>:<deadline>:<lifespan>:<liveliness>`,
    /// each component empty when it equals rmw_zenoh's default.
    pub(crate) fn encode(&self) -> String {
        let reliability = match self.reliability {
            Reliability::Reliable => "",
            Reliability::BestEffort => "2",
        };
        let durability = match self.durability {
            Durability::Volatile => "",
            Durability::TransientLocal => "1",
        };
        let history = match self.history {
            History::KeepLast => "",
            History::KeepAll => "2",
        };
        let depth = if self.depth == RMW_ZENOH_DEFAULT_DEPTH {
            String::new()
        } else {
            alloc::format!("{}", self.depth)
        };
        alloc::format!("{reliability}:{durability}:{history},{depth}:,:,:,,")
    }
}
