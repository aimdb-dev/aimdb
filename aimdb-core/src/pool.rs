//! Connector pool traits for MQTT, Kafka, HTTP, and other protocols
//!
//! Connector implementations provide pools that implement these traits,
//! enabling automatic consumer registration via `.with_connector_pool()`.

extern crate alloc;

use alloc::{boxed::Box, string::String, vec::Vec};
use core::future::Future;
use core::pin::Pin;

/// Configuration for MQTT publishing
///
/// This struct is designed to work in both `std` (Tokio) and `no_std` (Embassy)
/// environments. All fields are Copy-able to avoid allocations in the publish path.
///
/// # Broker Information
///
/// The broker_host and broker_port fields identify which MQTT broker to publish to.
/// These are extracted from the connector URL during setup and passed to the pool
/// implementation so it knows which client connection to use.
#[derive(Debug, Clone)]
pub struct MqttPublishConfig {
    /// Quality of Service level (0, 1, or 2)
    pub qos: u8,

    /// Whether to retain the message on the broker
    pub retain: bool,

    /// Optional timeout in milliseconds (primarily for Embassy environments)
    /// None means use default timeout behavior
    pub timeout_ms: Option<u32>,

    /// MQTT broker hostname or IP address
    /// Extracted from the connector URL (e.g., "mqtt://broker.example.com:1883")
    pub broker_host: String,

    /// MQTT broker port
    /// Default is 1883 for mqtt:// and 8883 for mqtts://
    pub broker_port: u16,
}

impl Default for MqttPublishConfig {
    fn default() -> Self {
        Self {
            qos: 0,
            retain: false,
            timeout_ms: Some(5000), // 5s default timeout
            broker_host: String::from("localhost"),
            broker_port: 1883,
        }
    }
}

/// Error that can occur during MQTT publishing
///
/// Uses an enum instead of String for better performance in `no_std` environments
/// and to enable defmt logging support in Embassy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishError {
    /// Failed to connect to broker
    ConnectionFailed,
    /// Message payload too large for buffer
    MessageTooLarge,
    /// Quality of Service level not supported
    UnsupportedQoS,
    /// Network timeout occurred
    Timeout,
    /// Buffer full, cannot queue message
    BufferFull,
    /// Invalid topic string
    InvalidTopic,
}

#[cfg(feature = "defmt")]
impl defmt::Format for PublishError {
    fn format(&self, f: defmt::Formatter) {
        match self {
            Self::ConnectionFailed => defmt::write!(f, "ConnectionFailed"),
            Self::MessageTooLarge => defmt::write!(f, "MessageTooLarge"),
            Self::UnsupportedQoS => defmt::write!(f, "UnsupportedQoS"),
            Self::Timeout => defmt::write!(f, "Timeout"),
            Self::BufferFull => defmt::write!(f, "BufferFull"),
            Self::InvalidTopic => defmt::write!(f, "InvalidTopic"),
        }
    }
}

#[cfg(feature = "std")]
impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectionFailed => write!(f, "Failed to connect to MQTT broker"),
            Self::MessageTooLarge => write!(f, "Message payload too large"),
            Self::UnsupportedQoS => write!(f, "QoS level not supported"),
            Self::Timeout => write!(f, "Network timeout"),
            Self::BufferFull => write!(f, "Buffer full, cannot queue message"),
            Self::InvalidTopic => write!(f, "Invalid MQTT topic"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for PublishError {}

/// Trait for MQTT connector pools
///
/// Implement this trait to enable automatic MQTT publishing when records
/// are configured with `mqtt://` or `mqtts://` URLs.
///
/// # Embassy Compatibility
///
/// This trait is designed to work in both `std` (Tokio) and `no_std` (Embassy)
/// environments by:
/// - Using slice borrows instead of Vec ownership (allows stack buffers)
/// - Using structured config instead of String key-value pairs (zero allocations)
/// - Using Copy-able error types instead of String (better for embedded)
///
/// # Example Implementation
///
/// ```rust,ignore
/// impl MqttConnectorPool for MyMqttPool {
///     fn publish(
///         &self,
///         topic: &str,
///         config: &MqttPublishConfig,
///         payload: &[u8],
///     ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
///         Box::pin(async move {
///             // Tokio: Can allocate Vec internally if needed
///             // Embassy: Use heapless::Vec or static buffers
///             self.client.publish(topic, config.qos, config.retain, payload).await
///                 .map_err(|_| PublishError::ConnectionFailed)
///         })
///     }
/// }
/// ```
pub trait MqttConnectorPool: Send + Sync {
    /// Publish a message to an MQTT topic
    ///
    /// # Arguments
    /// * `url` - Full MQTT URL including broker and topic (e.g., "mqtt://broker:1883/sensors/temp")
    /// * `config` - Publishing configuration (QoS, retain, timeout)
    /// * `payload` - Message payload as byte slice
    ///
    /// # Returns
    /// `Ok(())` on success, `PublishError` on failure
    ///
    /// # Implementation Notes
    ///
    /// The pool should parse the URL to extract:
    /// - Broker information (host, port, scheme) - used to select/create the right client
    /// - Topic path - used for the MQTT publish operation
    ///
    /// - **Tokio**: Can allocate Vec internally if needed for the client
    /// - **Embassy**: Should use heapless::Vec or static buffers
    /// - Timeout should be respected if `config.timeout_ms` is Some
    fn publish(
        &self,
        url: &str,
        config: &MqttPublishConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>>;
}

/// Trait for Kafka connector pools (placeholder for future implementation)
#[allow(dead_code)]
pub trait KafkaConnectorPool: Send + Sync {
    /// Publish a message to a Kafka topic
    fn publish(
        &self,
        url: &str,
        config: &[(String, String)],
        bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;
}

/// Trait for HTTP connector pools (placeholder for future implementation)
#[allow(dead_code)]
pub trait HttpConnectorPool: Send + Sync {
    /// Publish a message via HTTP POST
    fn publish(
        &self,
        url: &str,
        config: &[(String, String)],
        bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;

    // Mock MQTT pool for testing
    struct MockMqttPool;

    impl MqttConnectorPool for MockMqttPool {
        fn publish(
            &self,
            _url: &str,
            _config: &MqttPublishConfig,
            _payload: &[u8],
        ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }
    }

    #[test]
    fn test_mqtt_pool_trait() {
        let pool = Arc::new(MockMqttPool);

        // Verify the pool can be used as a trait object
        let _trait_obj: Arc<dyn MqttConnectorPool> = pool;
    }

    #[test]
    fn test_mqtt_publish_config_default() {
        let config = MqttPublishConfig::default();
        assert_eq!(config.qos, 0);
        assert!(!config.retain);
        assert_eq!(config.timeout_ms, Some(5000));
        assert_eq!(config.broker_host, "localhost");
        assert_eq!(config.broker_port, 1883);
    }

    #[test]
    fn test_publish_error_copy() {
        let err = PublishError::ConnectionFailed;
        let err2 = err; // Should be Copy
        assert_eq!(err, err2);
    }
}
