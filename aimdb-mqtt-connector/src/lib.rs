//! MQTT connector for AimDB
//!
//! Provides bidirectional MQTT integration for AimDB records:
//! - **Outbound**: Automatic publishing from AimDB to MQTT topics
//! - **Inbound**: Subscribe to MQTT topics and produce into AimDB buffers
//!
//! ## Features
//!
//! - `tokio-runtime`: Tokio-based connector using `rumqttc`
//! - `embassy-runtime`: Embassy connector for embedded systems using `mountain-mqtt`
//! - `embassy-tls`: TLS (`mqtts://`), broker authentication, DNS, and the
//!   SNTP time source for the Embassy connector
//! - `tracing`: Debug logging support (std)
//! - `defmt`: Debug logging support (no_std)
//!
//! ## Tokio Usage (Standard Library)
//!
//! ```no_run
//! use aimdb_core::AimDbBuilder;
//! use aimdb_mqtt_connector::{MqttConnector, MqttLinkExt, MqttOutboundLinkExt};
//! use aimdb_tokio_adapter::TokioAdapter;
//! use std::sync::Arc;
//!
//! # #[derive(Clone, Debug)] struct Temperature { celsius: f32 }
//! # #[derive(Clone, Debug)] struct TempCommand { target: f32 }
//! # async fn temperature_producer(
//! #     ctx: aimdb_core::RuntimeContext,
//! #     producer: aimdb_core::Producer<Temperature>,
//! # ) {}
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let runtime = Arc::new(TokioAdapter::new()?);
//!
//! let mut builder = AimDbBuilder::new()
//!     .runtime(runtime)
//!     .with_connector(MqttConnector::new("mqtt://localhost:1883"));
//!
//! // Outbound: publish to MQTT (QoS/retain via the MqttLinkExt traits)
//! builder.configure::<Temperature>("sensor.temp", |reg| {
//!     reg.source(temperature_producer)
//!        .link_to("mqtt://sensors/temperature")
//!        .with_qos(1)
//!        .with_retain(false)
//!        .with_serializer(|_ctx, t: &Temperature| Ok(t.celsius.to_be_bytes().to_vec()))
//!        .finish();
//! });
//!
//! // Inbound: subscribe from MQTT
//! builder.configure::<TempCommand>("command.temp", |reg| {
//!     reg.link_from("mqtt://commands/temperature")
//!        .with_deserializer(|_ctx, data| match data.try_into() {
//!            Ok(bytes) => Ok(TempCommand { target: f32::from_be_bytes(bytes) }),
//!            Err(_) => Err("bad frame".to_string()),
//!        })
//!        .finish();
//! });
//!
//! let (db, runner) = builder.build().await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Embassy Usage (Embedded)
//!
//! Illustrative (not compiled: requires the `embassy-runtime` feature and a
//! device network stack):
//!
//! ```rust,ignore
//! use aimdb_core::AimDbBuilder;
//! use aimdb_embassy_adapter::EmbassyAdapter;
//! use aimdb_mqtt_connector::embassy_client::MqttConnectorBuilder;
//! use alloc::sync::Arc;
//!
//! let runtime = Arc::new(EmbassyAdapter::new());
//!
//! let db = AimDbBuilder::new()
//!     .runtime(runtime)
//!     .with_connector(MqttConnectorBuilder::new("mqtt://192.168.1.100:1883", stack))
//!     .configure::<SensorData>(|reg| {
//!         reg.buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing)
//!            .source(sensor_producer)
//!            // Outbound: Publish to MQTT
//!            .link_to("mqtt://sensors/data")
//!            .with_serializer(|_ctx, data| postcard::to_vec(data).map_err(|_| /* ... */))
//!            .finish()
//!            // Inbound: Subscribe from MQTT
//!            .link_from("mqtt://commands/sensor")
//!            .with_deserializer(|_ctx, data| SensorCommand::from_bytes(data))
//!            .finish();
//!     })
//!     .build().await?;
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

// One `MqttConnector` over the `Native` and `Embedded` protocol backends.
pub mod connector;

// MQTT knobs over core's generic link builders (works on every feature leg).
pub mod link_ext;
pub use link_ext::{MqttLinkExt, MqttOutboundLinkExt};

// The `rumqttc` backend.
#[cfg(feature = "std")]
pub mod native;

// The `mountain-mqtt` backend: session loop, manager, and the TLS transport.
#[cfg(feature = "embedded")]
pub mod embedded;

// Deprecated module names, kept for one release so existing imports keep
// working. The modules no longer name a runtime.
#[cfg(feature = "std")]
#[deprecated(since = "0.7.0", note = "renamed to `native`")]
pub use crate::native as tokio_client;
#[cfg(feature = "embedded")]
#[deprecated(since = "0.7.0", note = "renamed to `embedded`")]
pub use crate::embedded as embassy_client;

#[cfg(feature = "embedded")]
pub use connector::Embedded;
#[cfg(feature = "embassy-tls")]
pub use connector::EmbeddedTls;
#[cfg(feature = "embassy-tls")]
pub use embedded::tls::TlsOptions;
pub use connector::{MqttConnector, Native};
