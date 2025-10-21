//! MQTT connector for AimDB
//!
//! This crate provides MQTT connectivity for AimDB records, supporting both
//! std (Tokio) and no_std (Embassy) environments.
//!
//! # Features
//!
//! - `tokio-runtime`: Enable Tokio-based MQTT connector using `rumqttc`
//! - `embassy-runtime`: Enable Embassy-based MQTT connector for embedded systems
//! - `tracing`: Enable tracing support for debugging
//!
//! # Architecture
//!
//! The connector spawns background tasks that:
//! 1. Subscribe to a record's buffer via emitter
//! 2. Transform values to MQTT payloads
//! 3. Publish to the configured MQTT broker
//! 4. Handle reconnection and error recovery
//!
//! # Example (Tokio)
//!
//! ```rust,ignore
//! use aimdb_core::{AimDb, AimDbBuilder};
//! use aimdb_tokio_adapter::TokioAdapter;
//! use std::sync::Arc;
//!
//! #[derive(Clone, Debug)]
//! struct Temperature {
//!     sensor_id: u32,
//!     celsius: f32,
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let runtime = Arc::new(TokioAdapter::new()?);
//!     
//!     let db = AimDb::build_with(runtime.clone(), |builder| {
//!         builder.configure::<Temperature>(|reg| {
//!             reg.producer(|_em, temp| async move {
//!                 println!("Temperature: {}°C", temp.celsius);
//!             })
//!             .link("mqtt://broker.example.com:1883/sensors/temperature")
//!                 .with_config("client_id", "sensor-001")
//!                 .with_config("qos", "1")
//!                 .finish();
//!         });
//!     })?;
//!     
//!     // Spawn MQTT connector tasks
//!     runtime.spawn_connectors(&db)?;
//!     
//!     // Publish temperature readings
//!     db.produce(Temperature { sensor_id: 1, celsius: 22.5 }).await?;
//!     
//!     Ok(())
//! }
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

use aimdb_core::connector::ConnectorUrl;

/// Errors that can occur in MQTT connector operations
#[derive(Debug, thiserror::Error)]
pub enum MqttError {
    /// Invalid MQTT URL format
    #[error("Invalid MQTT URL: {0}")]
    InvalidUrl(String),

    /// Failed to connect to MQTT broker
    #[error("Failed to connect to broker: {0}")]
    ConnectionFailed(String),

    /// Failed to publish message
    #[error("Failed to publish: {0}")]
    PublishFailed(String),

    /// Failed to subscribe to buffer
    #[error("Failed to subscribe to buffer: {0}")]
    SubscriptionFailed(String),

    /// Missing required configuration
    #[error("Missing required config: {0}")]
    MissingConfig(String),

    /// Database error
    #[error("Database error: {0}")]
    DbError(#[from] aimdb_core::DbError),
}

/// Result type for MQTT connector operations
pub type MqttResult<T> = Result<T, MqttError>;

/// Configuration for an MQTT connector
#[derive(Debug, Clone)]
pub struct MqttConfig {
    /// Parsed MQTT URL
    pub url: ConnectorUrl,
    
    /// MQTT client ID (optional, auto-generated if not provided)
    pub client_id: Option<String>,
    
    /// Quality of Service level (0, 1, or 2)
    pub qos: u8,
    
    /// Whether to retain messages
    pub retain: bool,
    
    /// Topic to publish to (extracted from URL path or config)
    pub topic: String,
}

impl MqttConfig {
    /// Create a new MQTT configuration from a connector URL
    pub fn from_url(url: ConnectorUrl) -> MqttResult<Self> {
        // Validate scheme
        if url.scheme() != "mqtt" && url.scheme() != "mqtts" {
            return Err(MqttError::InvalidUrl(format!(
                "Expected mqtt:// or mqtts://, got {}://",
                url.scheme()
            )));
        }

        // Extract topic from path (default to empty if not provided)
        let topic = url.path().trim_start_matches('/').to_string();
        if topic.is_empty() {
            return Err(MqttError::InvalidUrl(
                "MQTT URL must include topic path (e.g., mqtt://broker:1883/my/topic)".to_string()
            ));
        }

        Ok(Self {
            url,
            client_id: None,
            qos: 0,
            retain: false,
            topic,
        })
    }

    /// Set the client ID
    pub fn with_client_id(mut self, client_id: impl Into<String>) -> Self {
        self.client_id = Some(client_id.into());
        self
    }

    /// Set the QoS level
    pub fn with_qos(mut self, qos: u8) -> Self {
        self.qos = qos.min(2); // Cap at 2
        self
    }

    /// Set whether to retain messages
    pub fn with_retain(mut self, retain: bool) -> Self {
        self.retain = retain;
        self
    }
}

// Platform-specific implementations
#[cfg(feature = "tokio-runtime")]
pub mod tokio;

#[cfg(feature = "embassy-runtime")]
pub mod embassy;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mqtt_config_from_url() {
        let url = ConnectorUrl::parse("mqtt://broker.example.com:1883/sensors/temperature").unwrap();
        let config = MqttConfig::from_url(url).unwrap();

        assert_eq!(config.topic, "sensors/temperature");
        assert_eq!(config.qos, 0);
        assert_eq!(config.retain, false);
        assert!(config.client_id.is_none());
    }

    #[test]
    fn test_mqtt_config_invalid_scheme() {
        let url = ConnectorUrl::parse("http://broker.example.com:1883/topic").unwrap();
        let result = MqttConfig::from_url(url);

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), MqttError::InvalidUrl(_)));
    }

    #[test]
    fn test_mqtt_config_missing_topic() {
        let url = ConnectorUrl::parse("mqtt://broker.example.com:1883").unwrap();
        let result = MqttConfig::from_url(url);

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), MqttError::InvalidUrl(_)));
    }

    #[test]
    fn test_mqtt_config_builder() {
        let url = ConnectorUrl::parse("mqtt://broker:1883/topic").unwrap();
        let config = MqttConfig::from_url(url)
            .unwrap()
            .with_client_id("test-client")
            .with_qos(1)
            .with_retain(true);

        assert_eq!(config.client_id, Some("test-client".to_string()));
        assert_eq!(config.qos, 1);
        assert_eq!(config.retain, true);
    }

    #[test]
    fn test_mqtt_config_qos_capped() {
        let url = ConnectorUrl::parse("mqtt://broker:1883/topic").unwrap();
        let config = MqttConfig::from_url(url).unwrap().with_qos(5);

        assert_eq!(config.qos, 2); // Should be capped at 2
    }
}
