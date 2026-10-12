//! ROS 2 QoS overrides over core's generic link builders.
//!
//! ```rust,ignore
//! reg.link_to("ros2://cell4/temperature")
//!     .with_link_codec(link_codecs::Default)
//!     .with_depth(1)
//!     .with_reliability(Reliability::BestEffort)
//!     .finish();
//! ```

use std::string::ToString;

use aimdb_core::{InboundConnectorBuilder, OutboundConnectorBuilder};
use core::fmt::Debug;

use crate::ros2::{DEPTH_KEY, RELIABILITY_KEY};
use crate::Reliability;

/// The QoS a `ros2://` link advertises, beyond the rmw default profile
/// (`KEEP_LAST` 10, `RELIABLE`, `VOLATILE`).
pub trait Ros2LinkExt: Sized {
    /// `KEEP_LAST` with this depth (at least 1).
    fn with_depth(self, depth: u32) -> Self;

    /// The advertised reliability. The token carries it; this connector
    /// sends over Zenoh's default reliable path either way.
    fn with_reliability(self, reliability: Reliability) -> Self;
}

impl<'r, 'a, T> Ros2LinkExt for OutboundConnectorBuilder<'r, 'a, T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    fn with_depth(self, depth: u32) -> Self {
        self.with_config(DEPTH_KEY, &depth.to_string())
    }

    fn with_reliability(self, reliability: Reliability) -> Self {
        self.with_config(RELIABILITY_KEY, reliability.as_str())
    }
}

impl<'r, 'a, T> Ros2LinkExt for InboundConnectorBuilder<'r, 'a, T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    fn with_depth(self, depth: u32) -> Self {
        self.with_config(DEPTH_KEY, &depth.to_string())
    }

    fn with_reliability(self, reliability: Reliability) -> Self {
        self.with_config(RELIABILITY_KEY, reliability.as_str())
    }
}
