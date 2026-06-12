//! Transport connector traits for protocol-agnostic publishing
//!
//! Provides a generic `Connector` trait that enables scheme-based routing
//! to different transport protocols. Each connector manages a single connection
//! to a specific endpoint (e.g., one MQTT broker).
//!
//! # Design Philosophy
//!
//! - **Scheme-based routing**: the URL scheme (e.g. `mqtt://`, `knx://`) determines which connector handles requests
//! - **Single endpoint per connector**: Each connector connects to ONE broker/resource
//! - **Multi-transport publishing**: Same data can be published to multiple protocols
//! - **Protocol-agnostic core**: Core knows schemes and key/value options, never protocol semantics

use alloc::{boxed::Box, string::String, vec::Vec};
use core::future::Future;
use core::pin::Pin;

/// Protocol-agnostic connector configuration
///
/// Carries the route's key/value options to [`Connector::publish`]. Only the
/// genuinely protocol-agnostic `timeout_ms` is a typed field; every
/// protocol-specific knob (e.g. MQTT's `qos`/`retain`) travels in
/// [`protocol_options`](ConnectorConfig::protocol_options) and is interpreted
/// by the connector with its own defaults. Connector crates expose typed
/// setters as extension traits over the link builders (e.g. the MQTT
/// connector's `MqttLinkExt`).
#[derive(Debug, Clone)]
pub struct ConnectorConfig {
    /// Optional timeout in milliseconds for the publish/operation, as
    /// interpreted by the connector
    pub timeout_ms: Option<u32>,

    /// Protocol-specific options as key-value pairs
    /// Allows custom configuration without polluting the base struct
    pub protocol_options: Vec<(String, String)>,
}

impl Default for ConnectorConfig {
    fn default() -> Self {
        Self {
            timeout_ms: Some(5000),
            protocol_options: Vec::new(),
        }
    }
}

impl ConnectorConfig {
    /// Build a config from a route's URL-query key/value pairs.
    ///
    /// This is the shared seam the data-plane `pump_sink` helper uses to thread
    /// per-route configuration through to [`Connector::publish`] without changing
    /// the `publish` signature.
    ///
    /// Only the protocol-agnostic `timeout_ms` is lifted into the typed field;
    /// every other key is passed through verbatim in
    /// [`protocol_options`](ConnectorConfig::protocol_options) for the
    /// connector to interpret with its own defaults.
    pub fn from_query(query: &[(String, String)]) -> ConnectorConfig {
        let mut cfg = ConnectorConfig::default();
        for (k, v) in query {
            match k.as_str() {
                "timeout_ms" => {
                    if let Ok(n) = v.parse::<u32>() {
                        cfg.timeout_ms = Some(n);
                    }
                }
                _ => cfg.protocol_options.push((k.clone(), v.clone())),
            }
        }
        cfg
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
/// This trait enables multi-protocol publishing via scheme-based routing
/// (e.g. `mqtt://topic` → MQTT broker, `knx://1/0/6` → KNX group address).
///
/// Each connector manages ONE connection/endpoint. For multiple brokers/endpoints,
/// create multiple connectors and register them with different schemes.
///
/// # Example Implementation
///
/// Illustrative sketch (not compiled: the MQTT client types are fictional —
/// see `aimdb-mqtt-connector` for a real implementation):
///
/// ```rust,ignore
/// impl Connector for MqttConnector {
///     fn publish(
///         &self,
///         destination: &str,  // "sensors/temperature"
///         config: &ConnectorConfig,
///         payload: &[u8],
///     ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
///         // Protocol knobs come from the route's key/value options,
///         // with connector-chosen defaults.
///         let qos = config
///             .protocol_options
///             .iter()
///             .find(|(k, _)| k == "qos")
///             .and_then(|(_, v)| v.parse::<u8>().ok())
///             .unwrap_or(1);
///         Box::pin(async move {
///             self.client.publish(destination, qos, payload).await
///                 .map_err(|_| PublishError::ConnectionFailed)
///         })
///     }
/// }
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
    /// * `destination` - Protocol-specific path, no broker/host info
    ///   (e.g. an MQTT topic like "sensors/temperature")
    /// * `config` - Publishing configuration (timeout + protocol options)
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
        assert_eq!(config.timeout_ms, Some(5000));
        assert_eq!(config.protocol_options.len(), 0);
    }

    #[test]
    fn test_publish_error_copy() {
        let err = PublishError::ConnectionFailed;
        let err2 = err; // Should be Copy
        assert_eq!(err, err2);
    }
}
