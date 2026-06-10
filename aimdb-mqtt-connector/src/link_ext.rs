//! MQTT-specific knobs for the generic link builders
//!
//! Core's `OutboundConnectorBuilder`/`InboundConnectorBuilder` know schemes
//! and key/value options, never protocol semantics (design 034 §3.6). The
//! MQTT knobs live here as extension traits: importing them makes the MQTT
//! intent explicit at the call site, and the impls push the exact same
//! `("qos", …)` / `("retain", …)` option keys the MQTT clients have always
//! read from `protocol_options` — wire behavior is unchanged.
//!
//! ```rust,ignore
//! use aimdb_mqtt_connector::{MqttLinkExt, MqttOutboundLinkExt};
//!
//! reg.link_to("mqtt://sensors/temp")
//!     .with_qos(1)
//!     .with_retain(true)
//!     .with_serializer_raw(serialize)
//!     .finish();
//! ```

use aimdb_core::{InboundConnectorBuilder, OutboundConnectorBuilder};
use alloc::string::ToString;
use core::fmt::Debug;

/// MQTT knobs shared by outbound and inbound links.
pub trait MqttLinkExt: Sized {
    /// Sets the MQTT Quality of Service level (0, 1, or 2).
    ///
    /// Outbound: the publish QoS. Inbound: the subscribe QoS. Defaults to
    /// QoS 1 when unset (the connectors' own default).
    fn with_qos(self, qos: u8) -> Self;
}

/// MQTT knobs that only make sense when publishing.
pub trait MqttOutboundLinkExt: MqttLinkExt {
    /// Sets the MQTT retain flag (broker keeps the last message for
    /// late-joining subscribers). Defaults to `false` when unset.
    fn with_retain(self, retain: bool) -> Self;
}

impl<'r, 'a, T, R> MqttLinkExt for OutboundConnectorBuilder<'r, 'a, T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::RuntimeAdapter + 'static,
{
    fn with_qos(self, qos: u8) -> Self {
        self.with_config("qos", &qos.to_string())
    }
}

impl<'r, 'a, T, R> MqttOutboundLinkExt for OutboundConnectorBuilder<'r, 'a, T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::RuntimeAdapter + 'static,
{
    fn with_retain(self, retain: bool) -> Self {
        self.with_config("retain", if retain { "true" } else { "false" })
    }
}

impl<'r, 'a, T, R> MqttLinkExt for InboundConnectorBuilder<'r, 'a, T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
    R: aimdb_executor::RuntimeAdapter + 'static,
{
    fn with_qos(self, qos: u8) -> Self {
        self.with_config("qos", &qos.to_string())
    }
}
