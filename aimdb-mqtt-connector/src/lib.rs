//! MQTT connector for AimDB
//!
//! Provides bidirectional MQTT integration for AimDB records:
//! - **Outbound**: Automatic publishing from AimDB to MQTT topics
//! - **Inbound**: Subscribe to MQTT topics and produce into AimDB buffers
//!
//! ## Features
//!
//! The split is std vs `no_std`, not Tokio vs Embassy: the embedded backend
//! runs on any target that can supply a `StreamDialer`.
//!
//! - `std`: the `rumqttc` backend (QoS 0–2, platform trust roots)
//! - `embedded`: the `mountain-mqtt` backend over a caller-supplied transport;
//!   `alloc` only, with no executor, network stack or adapter
//! - `embedded-tls`: `mqtts://` via `embedded-tls`, on the same transport
//! - `embassy-runtime`: `embedded` plus the Embassy transport and clock
//! - `embassy-tls`: `embedded-tls` plus the SNTP time source, for a board with
//!   no RTC
//! - `critical-section-std-impl`: links a `critical-section` impl for std
//!   binaries, which the session channels need
//! - `tokio-runtime`: deprecated alias for `std`
//! - `tracing` / `defmt`: logging destinations
//!
//! ## Std Usage
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
//! ## Embedded Usage
//!
//! Illustrative (not compiled: requires the `embassy-runtime` feature and a
//! device network stack). The transport is what selects the backend — the same
//! call on any other adapter's dialer gets the same connector.
//!
//! ```rust,ignore
//! use aimdb_core::AimDbBuilder;
//! use aimdb_embassy_adapter::net::EmbassyNet;
//! use aimdb_embassy_adapter::EmbassyAdapter;
//! use aimdb_mqtt_connector::MqttConnector;
//! use alloc::sync::Arc;
//!
//! let runtime = Arc::new(EmbassyAdapter::new());
//!
//! let db = AimDbBuilder::new()
//!     .runtime(runtime)
//!     .with_connector(
//!         MqttConnector::new("mqtt://192.168.1.100:1883")
//!             .transport(EmbassyNet::tcp(stack, rx, tx))
//!             .with_client_id("my-unique-device-id"),
//!     )
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

// SNTP wire codec — pure and feature-independent so it is unit-tested on the
// host; only the TLS I/O task consumes it.
#[cfg_attr(not(feature = "embassy-tls"), allow(dead_code))]
pub(crate) mod sntp_codec;

// Deprecated module names, kept for one release so existing imports keep
// working. The modules no longer name a runtime.
#[cfg(feature = "embedded")]
#[deprecated(since = "0.7.0", note = "renamed to `embedded`")]
pub use crate::embedded as embassy_client;
#[cfg(feature = "std")]
#[deprecated(since = "0.7.0", note = "renamed to `native`")]
pub use crate::native as tokio_client;

#[cfg(feature = "embedded")]
pub use connector::Embedded;
#[cfg(feature = "embedded-tls")]
pub use connector::EmbeddedTls;
pub use connector::{MqttConnector, Native};
#[cfg(feature = "embedded-tls")]
pub use embedded::tls::TlsOptions;
