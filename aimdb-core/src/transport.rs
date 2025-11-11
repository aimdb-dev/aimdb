//! Transport connector traits for MQTT, Kafka, HTTP, shmem, and other protocols
//!
//! Provides a generic `Connector` trait that enables scheme-based routing
//! to different transport protocols. Each connector manages a single connection
//! to a specific endpoint (e.g., one MQTT broker, one shared memory segment, etc.).
//!
//! # Design Philosophy
//!
//! - **Scheme-based routing**: URL scheme (mqtt://, shmem://, kafka://) determines which connector handles requests
//! - **Single endpoint per connector**: Each connector connects to ONE broker/resource
//! - **Multi-transport publishing**: Same data can be published to multiple protocols
//! - **Protocol-agnostic core**: Core doesn't know about MQTT, Kafka, etc. - just routes by scheme

extern crate alloc;

use alloc::{boxed::Box, string::String, vec::Vec};
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use futures_core::Stream;

/// Protocol-agnostic connector configuration
///
/// Provides common configuration options that apply across multiple protocols.
/// Each protocol interprets these fields according to its semantics.
///
/// # Protocol Interpretation
///
/// - **MQTT**: qos=QoS level, retain=retain flag, timeout_ms=publish timeout
/// - **Kafka**: qos=acks setting (0=none, 1=leader, 2=all), timeout_ms=send timeout
/// - **HTTP**: qos=retry count, timeout_ms=request timeout
/// - **Shmem**: qos=priority, retain=pin in memory
#[derive(Debug, Clone)]
pub struct ConnectorConfig {
    /// Quality of Service / reliability level (0, 1, or 2)
    pub qos: u8,

    /// Whether to retain/persist the message
    pub retain: bool,

    /// Optional timeout in milliseconds
    pub timeout_ms: Option<u32>,

    /// Protocol-specific options as key-value pairs
    /// Allows custom configuration without polluting the base struct
    pub protocol_options: Vec<(String, String)>,
}

impl Default for ConnectorConfig {
    fn default() -> Self {
        Self {
            qos: 0,
            retain: false,
            timeout_ms: Some(5000),
            protocol_options: Vec::new(),
        }
    }
}

/// Error that can occur during connector publishing
///
/// Uses an enum instead of String for better performance in `no_std` environments
/// and to enable defmt logging support in Embassy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishError {
    /// Failed to connect to endpoint
    ConnectionFailed,
    /// Message payload too large for buffer
    MessageTooLarge,
    /// Quality of Service level not supported
    UnsupportedQoS,
    /// Network or operation timeout occurred
    Timeout,
    /// Buffer full, cannot queue message
    BufferFull,
    /// Invalid destination (topic, segment, endpoint)
    InvalidDestination,
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
            Self::InvalidDestination => defmt::write!(f, "InvalidDestination"),
        }
    }
}

#[cfg(feature = "std")]
impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectionFailed => write!(f, "Failed to connect to endpoint"),
            Self::MessageTooLarge => write!(f, "Message payload too large"),
            Self::UnsupportedQoS => write!(f, "QoS level not supported"),
            Self::Timeout => write!(f, "Operation timeout"),
            Self::BufferFull => write!(f, "Buffer full, cannot queue message"),
            Self::InvalidDestination => write!(f, "Invalid destination"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for PublishError {}

/// Generic transport connector trait for protocol-agnostic publishing
///
/// This trait enables multi-protocol publishing via scheme-based routing:
/// - `mqtt://topic` → MQTT broker
/// - `shmem://segment` → Shared memory
/// - `kafka://topic` → Kafka cluster
/// - `http://endpoint` → HTTP POST
/// - `dds://topic` → DDS topic
///
/// Each connector manages ONE connection/endpoint. For multiple brokers/endpoints,
/// create multiple connectors and register them with different schemes.
///
/// # Example Implementation
///
/// ```rust,ignore
/// impl Connector for MqttConnector {
///     fn publish(
///         &self,
///         destination: &str,  // "sensors/temperature"
///         config: &ConnectorConfig,
///         payload: &[u8],
///     ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
///         Box::pin(async move {
///             self.client.publish(destination, config.qos, config.retain, payload).await
///                 .map_err(|_| PublishError::ConnectionFailed)
///         })
///     }
/// }
/// ```
///
/// # Usage
///
/// ```rust,ignore
/// let mqtt_connector = MqttConnector::new("mqtt://broker.local:1883").await?;
///
/// let db = AimDbBuilder::new()
///     .runtime(runtime)
///     .with_connector("mqtt", Arc::new(mqtt_connector))
///     .configure::<Temperature>(|reg| {
///         reg.link("mqtt://sensors/temp")
///            .with_qos(1)
///            .finish()
///     })
///     .build()?;
/// ```
///
/// # Thread Safety
///
/// Requires Send + Sync for Tokio compatibility. For Embassy (single-threaded),
/// use `unsafe impl Send + Sync` with safety documentation.
pub trait Connector: Send + Sync {
    /// Publish data to a protocol-specific destination
    ///
    /// # Arguments
    /// * `destination` - Protocol-specific path (no broker/host info):
    ///   - MQTT: "sensors/temperature"
    ///   - Shmem: "temp_readings"
    ///   - Kafka: "production/events"
    ///   - HTTP: "api/v1/sensors"
    /// * `config` - Publishing configuration (QoS, retain, timeout, protocol options)
    /// * `payload` - Message payload as byte slice
    ///
    /// # Returns
    /// `Ok(())` on success, `PublishError` on failure
    fn publish(
        &self,
        destination: &str,
        config: &ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>>;

    /// Subscribe to data from external system (inbound: External → AimDB)
    ///
    /// Returns a stream of raw bytes from the external system.
    /// Each item represents a message/event that should be deserialized and
    /// published to an AimDB record.
    ///
    /// # Default Implementation
    ///
    /// Returns an empty stream (immediately ends). Connectors that don't support
    /// inbound data flow can use this default implementation.
    ///
    /// # Arguments
    /// * `source` - Protocol-specific subscription path:
    ///   - MQTT: "sensors/temperature" or "sensors/#" (wildcards)
    ///   - Kafka: "topic-name"
    ///   - WebSocket: "ws/events"
    /// * `config` - Subscription configuration (QoS, protocol options)
    ///
    /// # Returns
    /// A stream of `Result<Vec<u8>, PublishError>` where:
    /// - `Ok(bytes)` - Successfully received message payload
    /// - `Err(error)` - Connection error, deserialization failure, etc.
    ///
    /// # Example Implementation
    ///
    /// ```rust,ignore
    /// fn subscribe(
    ///     &self,
    ///     source: &str,
    ///     config: &ConnectorConfig,
    /// ) -> Pin<Box<dyn Stream<Item = Result<Vec<u8>, PublishError>> + Send + '_>> {
    ///     let stream = self.event_receiver
    ///         .filter_map(|event| match event {
    ///             Event::Message { payload, .. } => Some(Ok(payload)),
    ///             Event::Error(e) => Some(Err(e.into())),
    ///             _ => None,
    ///         });
    ///     Box::pin(stream)
    /// }
    /// ```
    fn subscribe(
        &self,
        _source: &str,
        _config: &ConnectorConfig,
    ) -> Pin<Box<dyn Stream<Item = Result<Vec<u8>, PublishError>> + Send + '_>> {
        /// Empty stream that immediately ends (no inbound support)
        struct EmptyStream;

        impl Stream for EmptyStream {
            type Item = Result<Vec<u8>, PublishError>;

            fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
                Poll::Ready(None)
            }
        }

        Box::pin(EmptyStream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;

    // Mock connector for testing
    struct MockConnector;

    impl Connector for MockConnector {
        fn publish(
            &self,
            _destination: &str,
            _config: &ConnectorConfig,
            _payload: &[u8],
        ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
            Box::pin(async move { Ok(()) })
        }
    }

    #[test]
    fn test_connector_trait() {
        let connector = Arc::new(MockConnector);

        // Verify the connector can be used as a trait object
        let _trait_obj: Arc<dyn Connector> = connector;
    }

    #[test]
    fn test_connector_config_default() {
        let config = ConnectorConfig::default();
        assert_eq!(config.qos, 0);
        assert!(!config.retain);
        assert_eq!(config.timeout_ms, Some(5000));
        assert_eq!(config.protocol_options.len(), 0);
    }

    #[test]
    fn test_publish_error_copy() {
        let err = PublishError::ConnectionFailed;
        let err2 = err; // Should be Copy
        assert_eq!(err, err2);
    }

    #[tokio::test]
    async fn test_connector_default_subscribe() {
        let connector = Arc::new(MockConnector);

        // Test that default subscribe() returns an empty stream
        let mut stream = connector.subscribe("test/topic", &ConnectorConfig::default());

        // Use StreamExt to test the stream
        use futures::StreamExt;

        // Empty stream should return None immediately
        let result = stream.next().await;
        assert!(result.is_none(), "Expected empty stream to return None");
    }
}
