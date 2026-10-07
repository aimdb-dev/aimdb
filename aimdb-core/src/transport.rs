//! Protocol-agnostic per-route configuration and publish errors.
//!
//! Core knows schemes and key/value options, never protocol semantics.

use alloc::{string::String, vec::Vec};

/// Protocol-agnostic connector configuration
///
/// Carries the route's key/value options to the connector
/// ([`RouteInfo::config`](crate::RouteInfo::config)). Only the
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

    /// The index of the record key that is setup together with the outbound route.
    /// The record key is unique, and its order in `AimDb::inner.storage` is immutable
    /// so this `record_index` could be used for `O(1)` lookup.
    pub record_index: Option<usize>,
}

impl Default for ConnectorConfig {
    fn default() -> Self {
        Self {
            timeout_ms: Some(5000),
            protocol_options: Vec::new(),
            record_index: None,
        }
    }
}

impl ConnectorConfig {
    /// Build a config from a route's URL-query key/value pairs.
    ///
    /// Only the protocol-agnostic `timeout_ms` and `record_index` (the last
    /// occurrence wins) are lifted
    /// into typed fields; every other key is passed through verbatim in
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
                "record_index" => {
                    if let Ok(i) = v.parse::<usize>() {
                        cfg.record_index = Some(i);
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

#[cfg(test)]
mod tests {
    use super::*;

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
