//! MQTT-specific knobs for the generic link builders
//!
//! Core's [`OutboundLinkBuilder`]/[`InboundLinkBuilder`] know schemes
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
//!
//! The knobs work on both forms of a link: attached (`link_to` / `link_from`)
//! and detached (`OutboundLink::new` / `InboundLink::new`, passed to
//! `reg.output(..)` / `reg.input(..)`):
//!
//! ```no_run
//! use aimdb_core::{InboundLink, OutboundLink};
//! use aimdb_mqtt_connector::{MqttLinkExt, MqttOutboundLinkExt};
//! # #[derive(Clone, Debug)] struct Temperature { celsius: f32 }
//! # fn wire(reg: &mut aimdb_core::RecordRegistrar<'_, Temperature>) {
//!
//! reg.input(
//!     InboundLink::new("mqtt://sensors/temp/in")
//!         .with_qos(1)
//!         .with_deserializer(|_ctx, b: &[u8]| {
//!             let bytes = b.try_into().map_err(|_| "need 4 bytes".to_string())?;
//!             Ok(Temperature { celsius: f32::from_be_bytes(bytes) })
//!         }),
//! )
//! .output(
//!     OutboundLink::new("mqtt://sensors/temp")
//!         .with_qos(1)
//!         .with_retain(false)
//!         .with_serializer(|_ctx, t: &Temperature| Ok(t.celsius.to_be_bytes().to_vec())),
//! );
//! # }
//! ```
//!
//! The knobs have no scheme of their own: on a connector registered under
//! another scheme ([`MqttConnector::scheme`](crate::MqttConnector::scheme)),
//! the same calls follow `reg.link_to("mqtt-cloud://sensors/temp")`.

use aimdb_core::{InboundLinkBuilder, OutboundLinkBuilder};
use alloc::string::ToString;
use core::fmt::Debug;

/// MQTT knobs shared by outbound and inbound links.
pub trait MqttLinkExt: Sized {
    /// Sets the MQTT Quality of Service level (0, 1, or 2).
    ///
    /// Outbound: the publish QoS, defaulting to 1.
    ///
    /// Inbound: the subscribe QoS, defaulting to 1. One subscribed filter can
    /// stand for several inbound links (a pattern covering an exact topic);
    /// it is subscribed at the highest QoS among them, so every link receives
    /// at least what it asked for.
    ///
    /// The `embedded` backend has no QoS 2: it publishes and subscribes a
    /// `qos=2` link at QoS 1 and warns once per such route at build. A value
    /// other than 0, 1 or 2 fails the build on both backends.
    fn with_qos(self, qos: u8) -> Self;
}

/// MQTT knobs that only make sense when publishing.
pub trait MqttOutboundLinkExt: MqttLinkExt {
    /// Sets the MQTT retain flag (broker keeps the last message for
    /// late-joining subscribers). Defaults to `false` when unset.
    fn with_retain(self, retain: bool) -> Self;
}

impl<T, R> MqttLinkExt for OutboundLinkBuilder<T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    fn with_qos(self, qos: u8) -> Self {
        self.with_config("qos", &qos.to_string())
    }
}

impl<T, R> MqttOutboundLinkExt for OutboundLinkBuilder<T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    fn with_retain(self, retain: bool) -> Self {
        self.with_config("retain", if retain { "true" } else { "false" })
    }
}

impl<T, R> MqttLinkExt for InboundLinkBuilder<T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    fn with_qos(self, qos: u8) -> Self {
        self.with_config("qos", &qos.to_string())
    }
}
