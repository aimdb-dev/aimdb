//! MQTT-specific knobs for the generic link builders
//!
//! Core's `OutboundConnectorBuilder`/`InboundConnectorBuilder` know schemes
//! and key/value options, never protocol semantics. The
//! MQTT knobs live here as extension traits: importing them makes the MQTT
//! intent explicit at the call site, and the impls push the exact same
//! `("qos", …)` / `("retain", …)` option keys the MQTT clients have always
//! read from `protocol_options` — wire behavior is unchanged.
//!
//! ```no_run
//! use aimdb_mqtt_connector::{MqttLinkExt, MqttOutboundLinkExt};
//! # #[derive(Clone, Debug)] struct Temperature { celsius: f32 }
//! # fn wire(reg: &mut aimdb_core::RecordRegistrar<'_, Temperature>) {
//!
//! reg.link_to("mqtt://sensors/temp")
//!     .with_qos(1)
//!     .with_retain(true)
//!     .with_serializer(|_ctx, t: &Temperature| Ok(t.celsius.to_be_bytes().to_vec()))
//!     .finish();
//! # }
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

impl<'r, 'a, T> MqttLinkExt for OutboundConnectorBuilder<'r, 'a, T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    fn with_qos(self, qos: u8) -> Self {
        self.with_config("qos", &qos.to_string())
    }
}

impl<'r, 'a, T> MqttOutboundLinkExt for OutboundConnectorBuilder<'r, 'a, T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    fn with_retain(self, retain: bool) -> Self {
        self.with_config("retain", if retain { "true" } else { "false" })
    }
}

impl<'r, 'a, T> MqttLinkExt for InboundConnectorBuilder<'r, 'a, T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    fn with_qos(self, qos: u8) -> Self {
        self.with_config("qos", &qos.to_string())
    }
}
