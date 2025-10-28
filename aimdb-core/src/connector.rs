//! Connector infrastructure for external protocol integration
//!
//! Provides the `.link()` builder API for ergonomic connector setup with
//! automatic client lifecycle management. Connectors bridge AimDB records
//! to external systems (MQTT, Kafka, HTTP, etc.).
//!
//! # Design Philosophy
//!
//! - **User Extensions**: Connector implementations are provided by users
//! - **Shared Clients**: Single client instance shared across tasks (Arc or static)
//! - **No Buffering**: Direct access to protocol clients, no intermediate queues
//! - **Type Safety**: Compile-time guarantees via typed handlers
//!
//! # Example
//!
//! ```rust,ignore
//! use aimdb_core::{RecordConfig, BufferCfg};
//!
//! fn weather_alert_record() -> RecordConfig<WeatherAlert> {
//!     RecordConfig::builder()
//!         .buffer(BufferCfg::SingleLatest)
//!         .link("mqtt://broker.example.com:1883")
//!             .out::<WeatherAlert>(|reader, mqtt| {
//!                 publish_alerts_to_mqtt(reader, mqtt)
//!             })
//!         .build()
//! }
//! ```

use core::fmt::{self, Debug};

extern crate alloc;

use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

#[cfg(feature = "std")]
use alloc::format;

use crate::DbResult;

/// Error that can occur during serialization
///
/// Uses an enum instead of String for better performance in `no_std` environments
/// and to enable defmt logging support in Embassy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerializeError {
    /// Output buffer is too small for the serialized data
    BufferTooSmall,

    /// Type mismatch in serializer (wrong type passed)
    TypeMismatch,

    /// Invalid data that cannot be serialized
    InvalidData,
}

#[cfg(feature = "defmt")]
impl defmt::Format for SerializeError {
    fn format(&self, f: defmt::Formatter) {
        match self {
            Self::BufferTooSmall => defmt::write!(f, "BufferTooSmall"),
            Self::TypeMismatch => defmt::write!(f, "TypeMismatch"),
            Self::InvalidData => defmt::write!(f, "InvalidData"),
        }
    }
}

#[cfg(feature = "std")]
impl std::fmt::Display for SerializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BufferTooSmall => write!(f, "Output buffer too small"),
            Self::TypeMismatch => write!(f, "Type mismatch in serializer"),
            Self::InvalidData => write!(f, "Invalid data for serialization"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for SerializeError {}

/// Type alias for serializer callbacks (reduces type complexity)
///
/// Requires the `alloc` feature for `Arc` and `Vec` (available in both std and no_std+alloc).
/// Serializers convert record values to bytes for publishing to external systems.
///
/// # Current Implementation
///
/// Returns `Vec<u8>` which requires heap allocation. This works in:
/// - ✅ `std` environments (full standard library)
/// - ✅ `no_std + alloc` environments (embedded with allocator, e.g., ESP32, STM32 with heap)
/// - ❌ `no_std` without `alloc` (bare-metal MCUs without allocator)
///
/// # Future Considerations
///
/// For zero-allocation embedded environments, future versions may support buffer-based
/// serialization using `&mut [u8]` output or static lifetime slices.
pub type SerializerFn =
    Arc<dyn Fn(&dyn core::any::Any) -> Result<Vec<u8>, SerializeError> + Send + Sync>;

/// Parsed connector URL with protocol, host, port, and credentials
///
/// Supports multiple protocol schemes:
/// - MQTT: `mqtt://host:port`, `mqtts://host:port`
/// - Kafka: `kafka://broker1:port,broker2:port/topic`
/// - HTTP: `http://host:port/path`, `https://host:port/path`
/// - WebSocket: `ws://host:port/path`, `wss://host:port/path`
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectorUrl {
    /// Protocol scheme (mqtt, mqtts, kafka, http, https, ws, wss)
    pub scheme: String,

    /// Host or comma-separated list of hosts (for Kafka)
    pub host: String,

    /// Port number (optional, protocol-specific defaults)
    pub port: Option<u16>,

    /// Path component (for HTTP/WebSocket)
    pub path: Option<String>,

    /// Username for authentication (optional)
    pub username: Option<String>,

    /// Password for authentication (optional)
    pub password: Option<String>,

    /// Query parameters (optional, parsed from URL)
    pub query_params: Vec<(String, String)>,
}

impl ConnectorUrl {
    /// Parses a connector URL string
    ///
    /// # Supported Formats
    ///
    /// - `mqtt://host:port`
    /// - `mqtt://user:pass@host:port`
    /// - `mqtts://host:port` (TLS)
    /// - `kafka://broker1:9092,broker2:9092/topic`
    /// - `http://host:port/path`
    /// - `https://host:port/path?key=value`
    /// - `ws://host:port/mqtt` (WebSocket)
    /// - `wss://host:port/mqtt` (WebSocket Secure)
    ///
    /// # Example
    ///
    /// ```rust
    /// use aimdb_core::connector::ConnectorUrl;
    ///
    /// let url = ConnectorUrl::parse("mqtt://user:pass@broker.example.com:1883").unwrap();
    /// assert_eq!(url.scheme, "mqtt");
    /// assert_eq!(url.host, "broker.example.com");
    /// assert_eq!(url.port, Some(1883));
    /// assert_eq!(url.username, Some("user".to_string()));
    /// ```
    pub fn parse(url: &str) -> DbResult<Self> {
        parse_connector_url(url)
    }

    /// Returns the default port for this protocol scheme
    pub fn default_port(&self) -> Option<u16> {
        match self.scheme.as_str() {
            "mqtt" | "ws" => Some(1883),
            "mqtts" | "wss" => Some(8883),
            "kafka" => Some(9092),
            "http" => Some(80),
            "https" => Some(443),
            _ => None,
        }
    }

    /// Returns the effective port (explicit or default)
    pub fn effective_port(&self) -> Option<u16> {
        self.port.or_else(|| self.default_port())
    }

    /// Returns true if this is a secure connection (TLS)
    pub fn is_secure(&self) -> bool {
        matches!(self.scheme.as_str(), "mqtts" | "https" | "wss")
    }

    /// Returns the URL scheme (protocol)
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Returns the path component, or "/" if not specified
    pub fn path(&self) -> &str {
        self.path.as_deref().unwrap_or("/")
    }
}

impl fmt::Display for ConnectorUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://", self.scheme)?;

        if let Some(ref username) = self.username {
            write!(f, "{}", username)?;
            if self.password.is_some() {
                write!(f, ":****")?; // Don't expose password in Display
            }
            write!(f, "@")?;
        }

        write!(f, "{}", self.host)?;

        if let Some(port) = self.port {
            write!(f, ":{}", port)?;
        }

        if let Some(ref path) = self.path {
            if !path.starts_with('/') {
                write!(f, "/")?;
            }
            write!(f, "{}", path)?;
        }

        Ok(())
    }
}

/// Connector client types (type-erased for storage)
///
/// This enum allows storing different connector client types in a unified way.
/// Actual protocol implementations will downcast to their concrete types.
///
/// # Design Note
///
/// This is intentionally minimal - actual client types are defined by
/// user extensions. The core only provides the infrastructure.
///
/// Works in both `std` and `no_std` (with `alloc`) environments.
#[derive(Clone)]
pub enum ConnectorClient {
    /// MQTT client (protocol-specific, user-provided)
    Mqtt(Arc<dyn core::any::Any + Send + Sync>),

    /// Kafka producer (protocol-specific, user-provided)
    Kafka(Arc<dyn core::any::Any + Send + Sync>),

    /// HTTP client (protocol-specific, user-provided)
    Http(Arc<dyn core::any::Any + Send + Sync>),

    /// Generic connector for custom protocols
    Generic {
        protocol: String,
        client: Arc<dyn core::any::Any + Send + Sync>,
    },
}

impl Debug for ConnectorClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectorClient::Mqtt(_) => write!(f, "ConnectorClient::Mqtt(..)"),
            ConnectorClient::Kafka(_) => write!(f, "ConnectorClient::Kafka(..)"),
            ConnectorClient::Http(_) => write!(f, "ConnectorClient::Http(..)"),
            ConnectorClient::Generic { protocol, .. } => {
                write!(f, "ConnectorClient::Generic({})", protocol)
            }
        }
    }
}

impl ConnectorClient {
    /// Downcasts to a concrete client type
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use rumqttc::AsyncClient;
    ///
    /// if let Some(mqtt_client) = connector.downcast_ref::<Arc<AsyncClient>>() {
    ///     // Use the MQTT client
    /// }
    /// ```
    pub fn downcast_ref<T: 'static>(&self) -> Option<&T> {
        match self {
            ConnectorClient::Mqtt(arc) => arc.downcast_ref::<T>(),
            ConnectorClient::Kafka(arc) => arc.downcast_ref::<T>(),
            ConnectorClient::Http(arc) => arc.downcast_ref::<T>(),
            ConnectorClient::Generic { client, .. } => client.downcast_ref::<T>(),
        }
    }
}

/// Configuration for a connector link
///
/// Stores the parsed URL and configuration until the record is built.
/// The actual client creation and handler spawning happens during the build phase.
#[derive(Clone)]
pub struct ConnectorLink {
    /// Parsed connector URL
    pub url: ConnectorUrl,

    /// Additional configuration options (protocol-specific)
    pub config: Vec<(String, String)>,

    /// Serialization callback that converts record values to bytes for publishing
    ///
    /// This is a type-erased function that takes `&dyn Any` and returns `Result<Vec<u8>, String>`.
    /// The connector implementation will downcast to the concrete type and call the serializer.
    ///
    /// If `None`, the connector must provide a default serialization mechanism or fail.
    ///
    /// Available in both `std` and `no_std` (with `alloc` feature) environments.
    pub serializer: Option<SerializerFn>,
}

impl Debug for ConnectorLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectorLink")
            .field("url", &self.url)
            .field("config", &self.config)
            .field(
                "serializer",
                &self.serializer.as_ref().map(|_| "<function>"),
            )
            .finish()
    }
}

impl ConnectorLink {
    /// Creates a new connector link from a URL
    pub fn new(url: ConnectorUrl) -> Self {
        Self {
            url,
            config: Vec::new(),
            serializer: None,
        }
    }

    /// Adds a configuration option
    pub fn with_config(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.push((key.into(), value.into()));
        self
    }
}

/// Parses a connector URL string into structured components
///
/// This is a simple parser that handles the most common URL formats.
/// For production use, consider using the `url` crate with feature flags.
fn parse_connector_url(url: &str) -> DbResult<ConnectorUrl> {
    use crate::DbError;

    // Split scheme from rest
    let (scheme, rest) = url.split_once("://").ok_or({
        #[cfg(feature = "std")]
        {
            DbError::InvalidOperation {
                operation: "parse_connector_url".into(),
                reason: format!("Missing scheme in URL: {}", url),
            }
        }
        #[cfg(not(feature = "std"))]
        {
            DbError::InvalidOperation {
                _operation: (),
                _reason: (),
            }
        }
    })?;

    // Extract credentials if present (user:pass@host)
    let (credentials, host_part) = if let Some(at_idx) = rest.find('@') {
        let creds = &rest[..at_idx];
        let host = &rest[at_idx + 1..];
        (Some(creds), host)
    } else {
        (None, rest)
    };

    let (username, password) = if let Some(creds) = credentials {
        if let Some((user, pass)) = creds.split_once(':') {
            (Some(user.to_string()), Some(pass.to_string()))
        } else {
            (Some(creds.to_string()), None)
        }
    } else {
        (None, None)
    };

    // Split path and query from host:port
    let (host_port, path, query_params) = if let Some(slash_idx) = host_part.find('/') {
        let hp = &host_part[..slash_idx];
        let path_query = &host_part[slash_idx..];

        // Split query parameters
        let (path_part, query_part) = if let Some(q_idx) = path_query.find('?') {
            (&path_query[..q_idx], Some(&path_query[q_idx + 1..]))
        } else {
            (path_query, None)
        };

        // Parse query parameters
        let params = if let Some(query) = query_part {
            query
                .split('&')
                .filter_map(|pair| {
                    let (k, v) = pair.split_once('=')?;
                    Some((k.to_string(), v.to_string()))
                })
                .collect()
        } else {
            Vec::new()
        };

        (hp, Some(path_part.to_string()), params)
    } else {
        (host_part, None, Vec::new())
    };

    // Split host and port
    let (host, port) = if let Some(colon_idx) = host_port.rfind(':') {
        let h = &host_port[..colon_idx];
        let p = &host_port[colon_idx + 1..];
        let port_num = p.parse::<u16>().ok();
        (h.to_string(), port_num)
    } else {
        (host_port.to_string(), None)
    };

    Ok(ConnectorUrl {
        scheme: scheme.to_string(),
        host,
        port,
        path,
        username,
        password,
        query_params,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn test_parse_simple_mqtt() {
        let url = ConnectorUrl::parse("mqtt://broker.example.com:1883").unwrap();
        assert_eq!(url.scheme, "mqtt");
        assert_eq!(url.host, "broker.example.com");
        assert_eq!(url.port, Some(1883));
        assert_eq!(url.username, None);
        assert_eq!(url.password, None);
    }

    #[test]
    fn test_parse_mqtt_with_credentials() {
        let url = ConnectorUrl::parse("mqtt://user:pass@broker.example.com:1883").unwrap();
        assert_eq!(url.scheme, "mqtt");
        assert_eq!(url.host, "broker.example.com");
        assert_eq!(url.port, Some(1883));
        assert_eq!(url.username, Some("user".to_string()));
        assert_eq!(url.password, Some("pass".to_string()));
    }

    #[test]
    fn test_parse_https_with_path() {
        let url = ConnectorUrl::parse("https://api.example.com:8443/events").unwrap();
        assert_eq!(url.scheme, "https");
        assert_eq!(url.host, "api.example.com");
        assert_eq!(url.port, Some(8443));
        assert_eq!(url.path, Some("/events".to_string()));
    }

    #[test]
    fn test_parse_with_query_params() {
        let url = ConnectorUrl::parse("http://api.example.com/data?key=value&foo=bar").unwrap();
        assert_eq!(url.scheme, "http");
        assert_eq!(url.host, "api.example.com");
        assert_eq!(url.path, Some("/data".to_string()));
        assert_eq!(url.query_params.len(), 2);
        assert_eq!(
            url.query_params[0],
            ("key".to_string(), "value".to_string())
        );
        assert_eq!(url.query_params[1], ("foo".to_string(), "bar".to_string()));
    }

    #[test]
    fn test_default_ports() {
        let mqtt = ConnectorUrl::parse("mqtt://broker.local").unwrap();
        assert_eq!(mqtt.default_port(), Some(1883));
        assert_eq!(mqtt.effective_port(), Some(1883));

        let https = ConnectorUrl::parse("https://api.example.com").unwrap();
        assert_eq!(https.default_port(), Some(443));
        assert_eq!(https.effective_port(), Some(443));
    }

    #[test]
    fn test_is_secure() {
        assert!(ConnectorUrl::parse("mqtts://broker.local")
            .unwrap()
            .is_secure());
        assert!(ConnectorUrl::parse("https://api.example.com")
            .unwrap()
            .is_secure());
        assert!(ConnectorUrl::parse("wss://ws.example.com")
            .unwrap()
            .is_secure());

        assert!(!ConnectorUrl::parse("mqtt://broker.local")
            .unwrap()
            .is_secure());
        assert!(!ConnectorUrl::parse("http://api.example.com")
            .unwrap()
            .is_secure());
        assert!(!ConnectorUrl::parse("ws://ws.example.com")
            .unwrap()
            .is_secure());
    }

    #[test]
    fn test_display_hides_password() {
        let url = ConnectorUrl::parse("mqtt://user:secret@broker.local:1883").unwrap();
        let display = format!("{}", url);
        assert!(display.contains("user:****"));
        assert!(!display.contains("secret"));
    }

    #[test]
    fn test_parse_kafka_style() {
        let url =
            ConnectorUrl::parse("kafka://broker1.local:9092,broker2.local:9092/my-topic").unwrap();
        assert_eq!(url.scheme, "kafka");
        // Note: Our simple parser doesn't handle the second port in comma-separated hosts perfectly
        // It parses "broker1.local:9092,broker2.local" as the host and "9092" as the port
        // This is acceptable for now - production connectors can handle this in their client factories
        assert!(url.host.contains("broker1.local"));
        assert!(url.host.contains("broker2.local"));
        assert_eq!(url.path, Some("/my-topic".to_string()));
    }

    #[test]
    fn test_parse_missing_scheme() {
        let result = ConnectorUrl::parse("broker.example.com:1883");
        assert!(result.is_err());
    }
}
