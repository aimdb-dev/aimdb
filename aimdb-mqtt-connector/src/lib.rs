//! MQTT connector for AimDB
//!
//! Provides MQTT publishing for AimDB records with automatic consumer registration.
//!
//! ## Features
//!
//! - `tokio-runtime`: Tokio-based connector using `rumqttc`
//! - `embassy-runtime`: Embassy connector for embedded systems (planned)
//! - `tracing`: Debug logging support
//!
//! ## Usage
//!
//! ```rust,ignore
//! use aimdb_core::AimDb;
//! use aimdb_mqtt_connector::MqttClientPool;
//! use std::sync::Arc;
//!
//! // Create pool and pass to builder
//! let pool = Arc::new(MqttClientPool::new());
//! let db = AimDb::build_with(runtime, |builder| {
//!     builder
//!         .with_connector_pool(pool)
//!         .configure::<Temperature>(|reg| {
//!             reg.producer(|_em, temp| async move { /* ... */ })
//!                .link("mqtt://broker:1883/sensors/temp")
//!                    .with_serializer(|t| serde_json::to_vec(t).map_err(|e| e.to_string()))
//!                    .finish();
//!         });
//! })?;
//!
//! // Publishing happens automatically
//! db.produce(Temperature { celsius: 22.5 }).await?;
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

use aimdb_core::connector::ConnectorUrl;

#[cfg(not(feature = "std"))]
use alloc::{
    format,
    string::{String, ToString},
};
#[cfg(feature = "std")]
use std::string::String;

/// Errors that can occur in MQTT connector operations
#[cfg(feature = "std")]
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

/// Errors that can occur in MQTT connector operations (no_std version)
#[cfg(not(feature = "std"))]
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum MqttError {
    /// Invalid MQTT URL format
    InvalidUrl(String),

    /// Failed to connect to MQTT broker
    ConnectionFailed(String),

    /// Failed to publish message
    PublishFailed(String),

    /// Failed to subscribe to buffer
    SubscriptionFailed(String),

    /// Missing required configuration
    MissingConfig(String),

    /// Database error
    DbError(aimdb_core::DbError),
}

#[cfg(not(feature = "std"))]
impl core::fmt::Display for MqttError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MqttError::InvalidUrl(s) => write!(f, "Invalid MQTT URL: {}", s),
            MqttError::ConnectionFailed(s) => write!(f, "Failed to connect to broker: {}", s),
            MqttError::PublishFailed(s) => write!(f, "Failed to publish: {}", s),
            MqttError::SubscriptionFailed(s) => write!(f, "Failed to subscribe to buffer: {}", s),
            MqttError::MissingConfig(s) => write!(f, "Missing required config: {}", s),
            MqttError::DbError(e) => write!(f, "Database error: {:?}", e),
        }
    }
}

#[cfg(not(feature = "std"))]
impl From<aimdb_core::DbError> for MqttError {
    fn from(err: aimdb_core::DbError) -> Self {
        MqttError::DbError(err)
    }
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
                "MQTT URL must include topic path (e.g., mqtt://broker:1883/my/topic)".to_string(),
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
pub mod tokio_client;

#[cfg(feature = "embassy-runtime")]
pub mod embassy_client;

// Re-export platform-specific types
// Both implementations export the same MqttClientPool name for API compatibility
// When both features are enabled (e.g., during testing), prefer tokio
#[cfg(all(feature = "tokio-runtime", not(feature = "embassy-runtime")))]
pub use tokio_client::MqttClientPool;

#[cfg(all(feature = "embassy-runtime", not(feature = "tokio-runtime")))]
pub use embassy_client::MqttClientPool;

// When both features are enabled, export both with different names
#[cfg(all(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub use tokio_client::MqttClientPool as TokioMqttClientPool;

#[cfg(all(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub use embassy_client::MqttClientPool as EmbassyMqttClientPool;

#[cfg(all(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub use tokio_client::MqttClientPool; // Default to tokio when both enabled

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mqtt_config_from_url() {
        let url =
            ConnectorUrl::parse("mqtt://broker.example.com:1883/sensors/temperature").unwrap();
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
