//! Connector pool traits for MQTT, Kafka, HTTP, and other protocols
//!
//! Connector implementations provide pools that implement these traits,
//! enabling automatic consumer registration via `.with_connector_pool()`.

extern crate alloc;

use alloc::{boxed::Box, string::String, vec::Vec};
use core::future::Future;
use core::pin::Pin;

/// Trait for MQTT connector pools
///
/// Implement this trait to enable automatic MQTT publishing when records
/// are configured with `mqtt://` or `mqtts://` URLs.
pub trait MqttConnectorPool: Send + Sync {
    /// Publish a message to an MQTT broker
    ///
    /// # Arguments
    /// * `url` - MQTT URL (e.g., "mqtt://broker:1883/topic")
    /// * `config` - Key-value pairs (e.g., qos, retain, client_id)
    /// * `bytes` - Serialized message bytes
    fn publish(
        &self,
        url: &str,
        config: &[(String, String)],
        bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;
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
            _config: &[(String, String)],
            _bytes: Vec<u8>,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }
    }

    #[test]
    fn test_mqtt_pool_trait() {
        let pool = Arc::new(MockMqttPool);

        // Verify the pool can be used as a trait object
        let _trait_obj: Arc<dyn MqttConnectorPool> = pool;
    }
}
