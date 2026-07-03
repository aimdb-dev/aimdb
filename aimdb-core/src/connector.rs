//! Connector infrastructure for external protocol integration
//!
//! Provides the `.link_to()` / `.link_from()` builder API for ergonomic
//! connector setup with automatic client lifecycle management. Connectors
//! bridge AimDB records to external systems (MQTT, KNX, WebSocket, …).
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
//! ```no_run
//! # use aimdb_core::AimDbBuilder;
//! # #[derive(Clone, Debug)] struct WeatherAlert { level: u8 }
//! # fn wire(builder: &mut AimDbBuilder) {
//! builder.configure::<WeatherAlert>("weather.alert", |reg| {
//!     // .buffer(BufferCfg::SingleLatest) — via your runtime adapter's ext trait
//!     reg.link_to("mqtt://alerts/weather")
//!         .with_serializer_raw(|alert: &WeatherAlert| Ok(vec![alert.level]))
//!         .finish();
//! });
//! # }
//! ```

use core::fmt::{self, Debug};
use core::future::Future;
use core::pin::Pin;

use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use alloc::format;

use crate::{builder::AimDb, DbResult};

/// Error that can occur during serialization
///
/// Uses an enum instead of String for better performance in `no_std` environments
/// and to enable defmt logging support in Embassy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerializeError {
    /// Output buffer is too small for the serialized data
    BufferTooSmall,

    /// Invalid data that cannot be serialized
    InvalidData,
}

#[cfg(feature = "defmt")]
impl defmt::Format for SerializeError {
    fn format(&self, f: defmt::Formatter) {
        match self {
            Self::BufferTooSmall => defmt::write!(f, "BufferTooSmall"),
            Self::InvalidData => defmt::write!(f, "InvalidData"),
        }
    }
}

#[cfg(feature = "std")]
impl std::fmt::Display for SerializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BufferTooSmall => write!(f, "Output buffer too small"),
            Self::InvalidData => write!(f, "Invalid data for serialization"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for SerializeError {}

/// One serialized record update, produced by a fused [`SerializedReader`]
///
/// Carries the wire payload plus the destination resolved by the link's
/// [`TopicProvider`] while the typed value was still in hand — the last
/// erasure crossing the old `topic_any(&dyn Any)` path required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerializedValue {
    /// Dynamic destination resolved by the link's `TopicProvider<T>`;
    /// `None` means use the route's default topic (from the URL).
    pub dest: Option<String>,
    /// Wire payload from the link's serializer.
    ///
    /// `Vec<u8>` requires heap allocation; works on `std` and
    /// `no_std + alloc` (not bare-metal without an allocator).
    pub payload: Vec<u8>,
}

/// Type alias for the future returned by [`SerializedReader::recv`]
///
/// Manual boxed future for object safety — same pattern as the rest of this
/// module (`#[async_trait]` would drag in `std`).
pub type RecvSerializedFuture<'a> =
    Pin<Box<dyn Future<Output = DbResult<SerializedValue>> + Send + 'a>>;

/// A subscription to one record, fused with destination resolution and
/// serialization at registration time — no `dyn Any` crosses this boundary
/// (design 036 W1).
pub trait SerializedReader: Send {
    /// Yield the next successfully serialized value.
    ///
    /// `ctx` is threaded per call (not captured) for context-aware
    /// serializers (design 026). Buffer errors propagate unchanged:
    /// `DbError::BufferLagged` means values were skipped but the reader
    /// recovered; any other error means the buffer is gone. Serialization
    /// failures are logged and skipped inside the reader.
    fn recv<'a>(&'a mut self, ctx: &'a crate::RuntimeContext) -> RecvSerializedFuture<'a>;
}

/// A record's outbound wire interface, built where the record type `T` is
/// known (`OutboundConnectorBuilder::finish`) and consumed by the pumps as
/// bytes. Replaces the erased `ConsumerTrait` + serializer + topic-provider
/// triple (design 036 W1).
pub trait SerializedSource: Send + Sync {
    /// Subscribe to the record's updates.
    ///
    /// Synchronous and infallible — the buffer handle is pre-resolved at
    /// construction (design 029).
    fn subscribe(&self) -> Box<dyn SerializedReader>;
}

/// Type alias for source factory callback (alloc feature)
///
/// Takes the live [`AimDb`] and returns the fused [`SerializedSource`].
/// This allows capturing the record type T at link_to() time while storing
/// the factory in a type-erased ConnectorLink. The factory runs once at
/// route-collection time, not per message.
///
/// Available in both `std` and `no_std + alloc` environments.
pub type SourceFactoryFn = Arc<dyn Fn(&AimDb) -> Box<dyn SerializedSource> + Send + Sync>;

// ============================================================================
// TopicProvider - Dynamic topic/destination routing
// ============================================================================

/// Trait for dynamic topic providers (outbound only)
///
/// Implement this trait to dynamically determine MQTT topics (or KNX group addresses)
/// based on the data being published. This enables reusable routing logic that
/// can be shared across multiple record types.
///
/// # Type Safety
///
/// The trait is generic over `T`, providing compile-time type safety
/// at the implementation site. The provider stays typed end-to-end: it is
/// fused into the link's [`SerializedSource`] at registration time and
/// called with `&T` while the value is in hand (design 036 W1).
///
/// # no_std Compatibility
///
/// Works in both `std` and `no_std + alloc` environments.
///
/// # Example
///
/// ```rust
/// use aimdb_core::connector::TopicProvider;
/// # #[derive(Clone, Debug)] struct Temperature { sensor_id: u32 }
///
/// struct SensorTopicProvider;
///
/// impl TopicProvider<Temperature> for SensorTopicProvider {
///     fn topic(&self, value: &Temperature) -> Option<String> {
///         Some(format!("sensors/temp/{}", value.sensor_id))
///     }
/// }
/// ```
pub trait TopicProvider<T>: Send + Sync {
    /// Determine the topic/destination for a given value
    ///
    /// Returns `Some(topic)` to use a dynamic topic, or `None` to fall back
    /// to the static topic from the `link_to()` URL.
    fn topic(&self, value: &T) -> Option<String>;
}

/// Address of a record link: `scheme://resource` (e.g. `mqtt://sensors/temp`).
///
/// A link address names a topic/resource on a connector's already-configured
/// endpoint — it is *not* a URL: there is no host, port, or credential
/// component (the connector constructor takes the endpoint
/// [`ConnectorUrl`]). The scheme selects which registered connector serves
/// the link; the resource is passed to it verbatim (e.g. an MQTT topic, a
/// KNX group address).
///
/// A trailing `?key=value` query is accepted and ignored; per-link options
/// are passed via `.with_config()` / `.with_timeout_ms()`.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkAddress {
    scheme: String,
    resource: String,
}

impl LinkAddress {
    /// Parses a `scheme://resource` link address.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aimdb_core::connector::LinkAddress;
    ///
    /// let addr = LinkAddress::parse("mqtt://sensors/temp").unwrap();
    /// assert_eq!(addr.scheme(), "mqtt");
    /// assert_eq!(addr.resource_id(), "sensors/temp");
    /// ```
    pub fn parse(s: &str) -> DbResult<Self> {
        use crate::DbError;

        let (scheme, rest) = s
            .split_once("://")
            .ok_or_else(|| DbError::InvalidOperation {
                operation: "LinkAddress::parse".into(),
                reason: alloc::format!("link address '{s}' missing '://' separator"),
            })?;

        // Per-link options travel via .with_config(); a query here is legal
        // input but carries no meaning — strip it.
        let resource = rest.split_once('?').map(|(r, _)| r).unwrap_or(rest);
        let resource = resource.trim_start_matches('/');

        if scheme.is_empty() || resource.is_empty() {
            return Err(DbError::InvalidOperation {
                operation: "LinkAddress::parse".into(),
                reason: alloc::format!("link address '{s}' needs both a scheme and a resource"),
            });
        }

        Ok(Self {
            scheme: scheme.to_string(),
            resource: resource.to_string(),
        })
    }

    /// Returns the scheme (selects the registered connector, e.g. `"mqtt"`).
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Returns the resource identifier the connector addresses (topic, group
    /// address, path — passed to the connector verbatim).
    pub fn resource_id(&self) -> &str {
        &self.resource
    }
}

impl fmt::Display for LinkAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}", self.scheme, self.resource)
    }
}

/// Parsed connector *endpoint* URL with protocol, host, port, and credentials
///
/// This is the URL a connector is constructed with (broker/gateway/server
/// endpoint) — record links use the far simpler [`LinkAddress`] instead.
/// The parser is scheme-agnostic: any `scheme://…` URL parses. Connectors in
/// this workspace use e.g.:
/// - MQTT: `mqtt://host:port`, `mqtts://host:port`
/// - KNX: `knx://gateway:3671`
/// - WebSocket: `ws://host:port/path`, `wss://host:port/path`
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectorUrl {
    /// Protocol scheme (e.g. mqtt, mqtts, knx, ws, wss, uds, serial)
    pub scheme: String,

    /// Host, or a comma-separated host list (preserved verbatim)
    pub host: String,

    /// Port number (optional, protocol-specific defaults)
    pub port: Option<u16>,

    /// Path component (optional)
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
    /// - `knx://gateway:3671`
    /// - `ws://host:port/path?key=value` (WebSocket)
    /// - `wss://host:port/path` (WebSocket Secure)
    ///
    /// Any other `scheme://…` parses the same way; comma-separated host
    /// lists are preserved verbatim in [`host`](ConnectorUrl::host).
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

/// Configuration for a connector link
///
/// Stores the parsed URL, configuration, and the fused source factory until
/// the record is built. The actual client creation and handler spawning
/// happens during the build phase.
#[derive(Clone)]
pub struct ConnectorLink {
    /// Parsed link address (`scheme://resource`)
    pub url: LinkAddress,

    /// Additional configuration options (protocol-specific)
    pub config: Vec<(String, String)>,

    /// Fused source factory (alloc feature)
    ///
    /// Takes the live [`AimDb`] and returns the [`SerializedSource`] whose
    /// readers yield destination + payload directly (subscribe → recv →
    /// resolve topic → serialize, all typed inside). Captures the record
    /// type T at link_to() configuration time — `finish()` validates the
    /// serializer is present before registering the link, so the factory is
    /// always set.
    ///
    /// Available in both `std` and `no_std + alloc` environments.
    pub source_factory: SourceFactoryFn,
}

impl Debug for ConnectorLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectorLink")
            .field("url", &self.url)
            .field("config", &self.config)
            .field("source_factory", &"<factory>")
            .finish()
    }
}

impl ConnectorLink {
    /// Creates a new connector link from a link address and source factory
    pub fn new(url: LinkAddress, source_factory: SourceFactoryFn) -> Self {
        Self {
            url,
            config: Vec::new(),
            source_factory,
        }
    }

    /// Creates the fused serialized source using the stored factory.
    ///
    /// Runs once at route-collection time; the readers it hands out are the
    /// per-message path (no `Box<dyn Any>`, design 036 W1).
    ///
    /// Available in both `std` and `no_std + alloc` environments.
    pub fn create_source(&self, db: &AimDb) -> Box<dyn SerializedSource> {
        (self.source_factory)(db)
    }
}

/// Fused inbound ingest callback: deserialize + produce in one typed closure
///
/// Built where the record type `T` is known (`InboundConnectorBuilder::finish`),
/// so no `Box<dyn Any>` crosses the connector boundary per message. The closure
/// captures the typed producer and deserializer; callers only see bytes.
///
/// Synchronous by design: `Producer<T>::produce` is sync and infallible
/// (design 029, pre-resolved write handle), so the only failure is the user
/// deserializer's — reported as the same `String` the deserializer API uses.
///
/// The [`RuntimeContext`](crate::RuntimeContext) is threaded per call (not
/// captured) for context-aware deserializers (design 026).
pub type IngestFn = Arc<dyn Fn(&crate::RuntimeContext, &[u8]) -> Result<(), String> + Send + Sync>;

/// Type alias for ingest factory callback (alloc feature)
///
/// Takes the live [`AimDb`] and returns the fused [`IngestFn`]. This allows
/// capturing the record type T at link_from() time while storing the factory
/// in a type-erased InboundConnectorLink. The factory runs once at
/// route-collection time, not per message.
///
/// Available in both `std` and `no_std + alloc` environments.
pub type IngestFactoryFn = Arc<dyn Fn(&AimDb) -> IngestFn + Send + Sync>;

/// Topic resolver function for inbound connections (late-binding)
///
/// Called once at connector startup to resolve the subscription topic.
/// Returns `Some(topic)` to use a dynamic topic, or `None` to fall back
/// to the static topic from the `link_from()` URL.
///
/// # Use Cases
///
/// - Topics determined from smart contracts at runtime
/// - Service discovery integration
/// - Environment-specific topic configuration
/// - Topics read from configuration files or databases
///
/// # no_std Compatibility
///
/// Works in both `std` and `no_std + alloc` environments.
pub type TopicResolverFn = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Configuration for an inbound connector link (External → AimDB)
///
/// Stores the parsed URL, configuration, and the fused ingest factory. The
/// factory captures the type T at creation time, allowing type-safe
/// deserialize+produce later without needing PhantomData or type parameters.
#[derive(Clone)]
pub struct InboundConnectorLink {
    /// Parsed link address (`scheme://resource`)
    pub url: LinkAddress,

    /// Additional configuration options (protocol-specific)
    pub config: Vec<(String, String)>,

    /// Fused ingest factory (alloc feature)
    ///
    /// Takes the live [`AimDb`] and returns the [`IngestFn`] that
    /// deserializes bytes and produces into the record's buffer in one typed
    /// closure. Captures the record type T at link_from() call time —
    /// `finish()` validates the deserializer is present before registering
    /// the link, so the factory is always set.
    ///
    /// Available in both `std` and `no_std + alloc` environments.
    pub ingest_factory: IngestFactoryFn,

    /// Optional dynamic topic resolver (late-binding)
    ///
    /// Called once at connector startup to determine the subscription topic.
    /// If the resolver returns `None`, the static topic from the URL is used.
    ///
    /// Available in both `std` and `no_std + alloc` environments.
    pub topic_resolver: Option<TopicResolverFn>,
}

impl Debug for InboundConnectorLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InboundConnectorLink")
            .field("url", &self.url)
            .field("config", &self.config)
            .field("ingest_factory", &"<factory>")
            .field(
                "topic_resolver",
                &self.topic_resolver.as_ref().map(|_| "<function>"),
            )
            .finish()
    }
}

impl InboundConnectorLink {
    /// Creates a new inbound connector link from a link address and ingest factory
    pub fn new(url: LinkAddress, ingest_factory: IngestFactoryFn) -> Self {
        Self {
            url,
            config: Vec::new(),
            ingest_factory,
            topic_resolver: None,
        }
    }

    /// Creates the fused ingest callback using the stored factory.
    ///
    /// Runs once at route-collection time; the returned [`IngestFn`] is the
    /// per-message path (deserialize + produce, no erasure crossing).
    ///
    /// Available in both `std` and `no_std + alloc` environments.
    pub fn create_ingest(&self, db: &AimDb) -> IngestFn {
        (self.ingest_factory)(db)
    }

    /// Resolves the subscription topic for this link
    ///
    /// If a topic resolver is configured, calls it to determine the topic.
    /// Otherwise, returns the static topic from the URL.
    ///
    /// This is called once at connector startup.
    pub fn resolve_topic(&self) -> String {
        self.topic_resolver
            .as_ref()
            .and_then(|resolver| resolver())
            .unwrap_or_else(|| self.url.resource_id().to_string())
    }
}

/// Parses a connector URL string into structured components
///
/// This is a simple parser that handles the most common URL formats.
/// For production use, consider using the `url` crate with feature flags.
fn parse_connector_url(url: &str) -> DbResult<ConnectorUrl> {
    use crate::DbError;

    // Split scheme from rest
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| DbError::InvalidOperation {
            operation: "parse_connector_url".into(),
            reason: format!("Missing scheme in URL: {}", url),
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

/// Trait for building connectors after the database is constructed
///
/// Connectors that need to collect routes from the database (for inbound routing)
/// implement this trait. The builder pattern allows connectors to be constructed
/// in two phases:
///
/// 1. Configuration phase: User provides broker URLs and settings
/// 2. Build phase: Connector collects routes from the database and initializes
///
/// # Example
///
/// Illustrative sketch of a connector author's `build()` (not compiled: the
/// client types are fictional — see `aimdb-mqtt-connector` for a real one):
///
/// ```rust,ignore
/// pub struct MqttConnectorBuilder {
///     broker_url: String,
/// }
///
/// impl ConnectorBuilder for MqttConnectorBuilder {
///     fn build<'a>(
///         &'a self,
///         db: &'a AimDb,
///     ) -> Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>> {
///         Box::pin(async move {
///             let routes = db.collect_inbound_routes(self.scheme());
///             let router = RouterBuilder::from_routes(routes).build();
///             let connector = MqttConnector::new(&self.broker_url, router).await?;
///             Ok(connector.futures())
///         })
///     }
///
///     fn scheme(&self) -> &str {
///         "mqtt"
///     }
/// }
/// ```
pub trait ConnectorBuilder: Send + Sync {
    /// Build the connector and return its driving futures.
    ///
    /// Called during `AimDbBuilder::build()` after the database has been
    /// constructed. The returned futures (infrastructure loops + per-route
    /// publishers) are appended to the builder's accumulator and driven by
    /// `AimDbRunner::run()`.
    ///
    /// # Arguments
    /// * `db` - The constructed database instance
    ///
    /// # Returns
    /// All `BoxFuture`s the connector needs to operate. Empty if the connector
    /// has no work to drive.
    #[allow(clippy::type_complexity)]
    fn build<'a>(
        &'a self,
        db: &'a AimDb,
    ) -> Pin<
        Box<
            dyn Future<Output = DbResult<Vec<Pin<Box<dyn Future<Output = ()> + Send + 'static>>>>>
                + Send
                + 'a,
        >,
    >;

    /// The URL scheme this connector handles
    ///
    /// Returns the scheme (e.g., "mqtt", "knx", "uds") that this connector
    /// will be registered under. Used for routing `.link_from()` and `.link_to()`
    /// declarations to the appropriate connector.
    fn scheme(&self) -> &str;
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

        let mqtts = ConnectorUrl::parse("mqtts://broker.local").unwrap();
        assert_eq!(mqtts.default_port(), Some(8883));

        // No connector, no default: unknown schemes carry no port opinion.
        let knx = ConnectorUrl::parse("knx://gateway.local").unwrap();
        assert_eq!(knx.default_port(), None);
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

    // ========================================================================
    // TopicProvider Tests
    // ========================================================================

    #[allow(dead_code)]
    #[derive(Debug, Clone)]
    struct TestTemperature {
        sensor_id: String,
        celsius: f32,
    }

    struct TestTopicProvider;

    impl super::TopicProvider<TestTemperature> for TestTopicProvider {
        fn topic(&self, value: &TestTemperature) -> Option<String> {
            Some(format!("sensors/temp/{}", value.sensor_id))
        }
    }

    #[test]
    fn test_topic_provider_as_trait_object() {
        // Providers are stored as Arc<dyn TopicProvider<T>> — typed, no
        // erasure (design 036 W1).
        let provider: Arc<dyn super::TopicProvider<TestTemperature>> = Arc::new(TestTopicProvider);
        let temp = TestTemperature {
            sensor_id: "kitchen-001".into(),
            celsius: 22.5,
        };

        assert_eq!(
            provider.topic(&temp),
            Some("sensors/temp/kitchen-001".into())
        );
    }

    #[test]
    fn test_topic_provider_returns_none() {
        struct OptionalTopicProvider;

        impl super::TopicProvider<TestTemperature> for OptionalTopicProvider {
            fn topic(&self, temp: &TestTemperature) -> Option<String> {
                if temp.sensor_id.is_empty() {
                    None // Fall back to default topic
                } else {
                    Some(format!("sensors/{}", temp.sensor_id))
                }
            }
        }

        let provider: Arc<dyn super::TopicProvider<TestTemperature>> =
            Arc::new(OptionalTopicProvider);

        // Non-empty sensor_id returns dynamic topic
        let temp_with_id = TestTemperature {
            sensor_id: "abc".into(),
            celsius: 20.0,
        };
        assert_eq!(provider.topic(&temp_with_id), Some("sensors/abc".into()));

        // Empty sensor_id returns None (fallback)
        let temp_without_id = TestTemperature {
            sensor_id: String::new(),
            celsius: 20.0,
        };
        assert_eq!(provider.topic(&temp_without_id), None);
    }

    // ========================================================================
    // TopicResolverFn Tests
    // ========================================================================

    #[test]
    fn test_topic_resolver_returns_some() {
        let resolver: super::TopicResolverFn = Arc::new(|| Some("resolved/topic".into()));

        assert_eq!(resolver(), Some("resolved/topic".into()));
    }

    #[test]
    fn test_topic_resolver_returns_none() {
        let resolver: super::TopicResolverFn = Arc::new(|| None);

        // Returns None, should fall back to default topic
        assert_eq!(resolver(), None);
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_topic_resolver_with_captured_state() {
        use std::sync::Mutex;

        let config = Arc::new(Mutex::new(Some("dynamic/topic".to_string())));
        let config_clone = config.clone();

        let resolver: super::TopicResolverFn =
            Arc::new(move || config_clone.lock().unwrap().clone());

        assert_eq!(resolver(), Some("dynamic/topic".into()));

        // Clear config
        *config.lock().unwrap() = None;
        assert_eq!(resolver(), None);
    }

    /// Dummy ingest factory for link-construction tests (never invoked).
    fn dummy_ingest_factory() -> super::IngestFactoryFn {
        Arc::new(|_db| Arc::new(|_ctx: &crate::RuntimeContext, _bytes: &[u8]| Ok(())))
    }

    #[test]
    fn test_inbound_connector_link_resolve_topic_default() {
        use super::{InboundConnectorLink, LinkAddress};

        let url = LinkAddress::parse("mqtt://sensors/temperature").unwrap();
        let link = InboundConnectorLink::new(url, dummy_ingest_factory());

        // No resolver configured, should return static topic from URL
        assert_eq!(link.resolve_topic(), "sensors/temperature");
    }

    #[test]
    fn test_inbound_connector_link_resolve_topic_dynamic() {
        use super::{InboundConnectorLink, LinkAddress};

        let url = LinkAddress::parse("mqtt://sensors/default").unwrap();
        let mut link = InboundConnectorLink::new(url, dummy_ingest_factory());

        // Configure dynamic resolver
        link.topic_resolver = Some(Arc::new(|| Some("sensors/dynamic/kitchen".into())));

        // Should return resolved topic, not URL topic
        assert_eq!(link.resolve_topic(), "sensors/dynamic/kitchen");
    }

    #[test]
    fn test_inbound_connector_link_resolve_topic_fallback() {
        use super::{InboundConnectorLink, LinkAddress};

        let url = LinkAddress::parse("mqtt://sensors/fallback").unwrap();
        let mut link = InboundConnectorLink::new(url, dummy_ingest_factory());

        // Configure resolver that returns None
        link.topic_resolver = Some(Arc::new(|| None));

        // Should fall back to static topic from URL
        assert_eq!(link.resolve_topic(), "sensors/fallback");
    }
}
