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
//! - `tracing`: Debug logging support (std)
//! - `defmt`: Debug logging support (no_std)
//!
//! ## Tokio Usage (Standard Library)
//!
//! ```rust,ignore
//! use aimdb_core::AimDbBuilder;
//! use aimdb_tokio_adapter::TokioAdapter;
//! use aimdb_mqtt_connector::MqttConnector;
//! use std::sync::Arc;
//!
//! let runtime = Arc::new(TokioAdapter::new()?);
//!
//! let db = AimDbBuilder::new()
//!     .runtime(runtime)
//!     .with_connector(MqttConnector::new("mqtt://localhost:1883"))
//!     .configure::<Temperature>(|reg| {
//!         reg.source(temperature_producer)
//!            // Outbound: Publish to MQTT
//!            .link_to("mqtt://sensors/temperature")
//!            .with_serializer(|t| {
//!                serde_json::to_vec(t)
//!                    .map_err(|_| aimdb_core::connector::SerializeError::InvalidData)
//!            })
//!            .finish()
//!            // Inbound: Subscribe from MQTT
//!            .link_from("mqtt://commands/temperature")
//!            .with_deserializer(|data| Temperature::from_json(data))
//!            .finish();
//!     })
//!     .build().await?;
//! ```
//!
//! ## Embassy Usage (Embedded)
//!
//! ```rust,ignore
//! use aimdb_core::AimDbBuilder;
//! use aimdb_embassy_adapter::EmbassyAdapter;
//! use aimdb_mqtt_connector::embassy_client::MqttConnectorBuilder;
//! use alloc::sync::Arc;
//!
//! let runtime = Arc::new(EmbassyAdapter::new_with_network(spawner, stack));
//!
//! let db = AimDbBuilder::new()
//!     .runtime(runtime)
//!     .with_connector(MqttConnectorBuilder::new("mqtt://192.168.1.100:1883"))
//!     .configure::<SensorData>(|reg| {
//!         reg.buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing)
//!            .source(sensor_producer)
//!            // Outbound: Publish to MQTT
//!            .link_to("mqtt://sensors/data")
//!            .with_serializer(|data| postcard::to_vec(data).map_err(|_| /* ... */))
//!            .finish()
//!            // Inbound: Subscribe from MQTT
//!            .link_from("mqtt://commands/sensor")
//!            .with_deserializer(|data| SensorCommand::from_bytes(data))
//!            .finish();
//!     })
//!     .build().await?;
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

// Platform-specific implementations
#[cfg(feature = "tokio-runtime")]
pub mod tokio_client;

#[cfg(feature = "embassy-runtime")]
pub mod embassy_client;

// Re-export platform-specific types
// Both implementations use MqttConnectorBuilder for API consistency
// When both features are enabled (e.g., during testing), prefer tokio
#[cfg(all(feature = "tokio-runtime", not(feature = "embassy-runtime")))]
pub use tokio_client::MqttConnectorBuilder as MqttConnector;

#[cfg(all(feature = "embassy-runtime", not(feature = "tokio-runtime")))]
pub use embassy_client::MqttConnectorBuilder as MqttConnector;

// When both features are enabled, export both with different names
#[cfg(all(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub use tokio_client::MqttConnectorBuilder as TokioMqttConnector;

#[cfg(all(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub use embassy_client::MqttConnectorBuilder as EmbassyMqttConnector;

#[cfg(all(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub use tokio_client::MqttConnectorBuilder as MqttConnector; // Default to tokio when both enabled
