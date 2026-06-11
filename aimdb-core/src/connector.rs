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
//!         .link_to("mqtt://broker.example.com:1883")
//!             .out::<WeatherAlert>(|reader, mqtt| {
//!                 publish_alerts_to_mqtt(reader, mqtt)
//!             })
//!         .build()
//! }
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

/// Type alias for context-aware type-erased serializer callbacks
///
/// Like `SerializerFn`, but receives the concrete [`RuntimeContext`](crate::RuntimeContext)
/// for platform-independent timestamps and logging during serialization.
pub type ContextSerializerFn = Arc<
    dyn Fn(crate::RuntimeContext, &dyn core::any::Any) -> Result<Vec<u8>, SerializeError>
        + Send
        + Sync,
>;

/// Which serializer variant is registered for an outbound link
///
/// Enforces mutual exclusivity between raw value-only serializers
/// and context-aware serializers.
#[derive(Clone)]
pub enum SerializerKind {
    /// Plain value-only serializer (from `.with_serializer_raw()`)
    Raw(SerializerFn),
    /// Context-aware serializer (from `.with_serializer()`)
    Context(ContextSerializerFn),
}

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
/// at the implementation site. Type erasure occurs only at storage time.
///
/// # no_std Compatibility
///
/// Works in both `std` and `no_std + alloc` environments.
///
/// # Example
///
/// ```rust,ignore
/// use aimdb_core::connector::TopicProvider;
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

/// Type-erased topic provider trait (internal)
///
/// Allows storing providers for different types in a unified collection.
/// The concrete type is recovered via `Any::downcast_ref()` at runtime.
pub trait TopicProviderAny: Send + Sync {
    /// Get the topic for a type-erased value
    ///
    /// Returns `None` if the value type doesn't match or if the provider
    /// returns `None` for this value.
    fn topic_any(&self, value: &dyn core::any::Any) -> Option<String>;
}

/// Wrapper struct for type-erasing a `TopicProvider<T>`
///
/// This wraps a concrete `TopicProvider<T>` implementation and provides
/// the `TopicProviderAny` interface for type-erased storage.
///
/// Uses `PhantomData<fn(T) -> T>` instead of `PhantomData<T>` to avoid
/// inheriting Send/Sync bounds from T (the data type isn't stored).
pub struct TopicProviderWrapper<T, P>
where
    T: 'static,
    P: TopicProvider<T>,
{
    provider: P,
    // Use fn(T) -> T to avoid Send/Sync variance issues with T
    _phantom: core::marker::PhantomData<fn(T) -> T>,
}

impl<T, P> TopicProviderWrapper<T, P>
where
    T: 'static,
    P: TopicProvider<T>,
{
    /// Create a new wrapper for a topic provider
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            _phantom: core::marker::PhantomData,
        }
    }
}

impl<T, P> TopicProviderAny for TopicProviderWrapper<T, P>
where
    T: 'static,
    P: TopicProvider<T> + Send + Sync,
{
    fn topic_any(&self, value: &dyn core::any::Any) -> Option<String> {
        value
            .downcast_ref::<T>()
            .and_then(|v| self.provider.topic(v))
    }
}

/// Type alias for stored topic provider (no_std compatible)
///
/// Uses `Arc<dyn TopicProviderAny>` for shared ownership across async tasks.
pub type TopicProviderFn = Arc<dyn TopicProviderAny>;

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

    /// Returns the resource identifier for protocols where the URL specifies a topic/key
    ///
    /// This is designed for the simplified connector model where each connector manages
    /// a single broker/server connection, and URLs only specify the resource (topic, key, path).
    ///
    /// # Examples
    ///
    /// - `mqtt://commands/temperature` → `"commands/temperature"` (topic)
    /// - `mqtt://sensors/temp` → `"sensors/temp"` (topic)
    /// - `kafka://events` → `"events"` (topic)
    ///
    /// The format is `scheme://resource` where resource = host + path combined.
    pub fn resource_id(&self) -> String {
        let path = self.path().trim_start_matches('/');

        // Combine host and path to form the complete resource identifier
        // For mqtt://commands/temperature: host="commands", path="/temperature"
        // Result: "commands/temperature"
        if !self.host.is_empty() && !path.is_empty() {
            alloc::format!("{}/{}", self.host, path)
        } else if !self.host.is_empty() {
            self.host.clone()
        } else if !path.is_empty() {
            path.to_string()
        } else {
            String::new()
        }
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
    /// Either a plain value-only serializer (`Raw`) or a context-aware
    /// serializer (`Context`) that receives `RuntimeContext` for timestamps
    /// and logging.
    ///
    /// If `None`, the connector must provide a default serialization mechanism or fail.
    ///
    /// Available in both `std` and `no_std` (with `alloc` feature) environments.
    pub serializer: Option<SerializerKind>,

    /// Consumer factory callback (alloc feature)
    ///
    /// Creates `ConsumerTrait` from the live [`AimDb`] to enable type-safe subscription.
    /// The factory captures the record type T at link_to() configuration time,
    /// allowing the connector to subscribe without knowing T at compile time.
    ///
    /// Mirrors the producer_factory pattern used for inbound connectors.
    ///
    /// Available in both `std` and `no_std + alloc` environments.
    pub consumer_factory: Option<ConsumerFactoryFn>,

    /// Optional dynamic topic provider
    ///
    /// When set, the provider is called with each value to determine the
    /// topic/destination dynamically. If the provider returns `None`,
    /// the static topic from the URL is used as fallback.
    ///
    /// Available in both `std` and `no_std + alloc` environments.
    pub topic_provider: Option<TopicProviderFn>,
}

impl Debug for ConnectorLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectorLink")
            .field("url", &self.url)
            .field("config", &self.config)
            .field(
                "serializer",
                &self.serializer.as_ref().map(|s| match s {
                    SerializerKind::Raw(_) => "<raw>",
                    SerializerKind::Context(_) => "<context>",
                }),
            )
            .field(
                "consumer_factory",
                &self.consumer_factory.as_ref().map(|_| "<function>"),
            )
            .field(
                "topic_provider",
                &self.topic_provider.as_ref().map(|_| "<function>"),
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
            consumer_factory: None,
            topic_provider: None,
        }
    }

    /// Adds a configuration option
    pub fn with_config(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.push((key.into(), value.into()));
        self
    }

    /// Creates a consumer using the stored factory (alloc feature)
    ///
    /// Invokes the consumer factory with the live database to create a
    /// ConsumerTrait instance. Returns None if no factory is configured.
    ///
    /// Available in both `std` and `no_std + alloc` environments.
    pub fn create_consumer(&self, db: &AimDb) -> Option<Box<dyn ConsumerTrait>> {
        self.consumer_factory.as_ref().map(|f| f(db))
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

/// Type alias for consumer factory callback (alloc feature)
///
/// Takes the live [`AimDb`] and returns a boxed `ConsumerTrait`. This allows
/// capturing the record type T at link_to() time while storing the factory
/// in a type-erased ConnectorLink.
///
/// Mirrors the ProducerFactoryFn pattern for symmetry between inbound and outbound.
///
/// Available in both `std` and `no_std + alloc` environments.
pub type ConsumerFactoryFn = Arc<dyn Fn(&AimDb) -> Box<dyn ConsumerTrait> + Send + Sync>;

/// Type-erased consumer trait for outbound routing
///
/// Mirrors ProducerTrait but for consumption. Allows connectors to subscribe
/// to typed values without knowing the concrete type T at compile time.
///
/// # Implementation Note
///
/// Like ProducerTrait, this uses manual futures instead of `#[async_trait]`
/// to enable `no_std` compatibility.
pub trait ConsumerTrait: Send + Sync {
    /// Subscribe to typed values from this record
    ///
    /// Returns a type-erased reader that can be polled for `Box<dyn Any>` values.
    /// The connector will downcast to the expected type after deserialization.
    /// Infallible since M14 — subscription is a pre-resolved buffer handle.
    fn subscribe_any<'a>(&'a self) -> SubscribeAnyFuture<'a>;
}

/// Type alias for the future returned by `ConsumerTrait::subscribe_any`
type SubscribeAnyFuture<'a> = Pin<Box<dyn Future<Output = Box<dyn AnyReader>> + Send + 'a>>;

/// Type alias for the future returned by `AnyReader::recv_any`
type RecvAnyFuture<'a> =
    Pin<Box<dyn Future<Output = DbResult<Box<dyn core::any::Any + Send>>> + Send + 'a>>;

/// Helper trait for type-erased reading
///
/// Allows reading values from a buffer without knowing the concrete type at compile time.
/// The value is returned as `Box<dyn Any>` and must be downcast by the caller.
pub trait AnyReader: Send {
    /// Receive a type-erased value from the buffer
    ///
    /// Returns `Box<dyn Any>` which must be downcast to the concrete type.
    /// Returns an error if the buffer is closed or an I/O error occurs.
    fn recv_any<'a>(&'a mut self) -> RecvAnyFuture<'a>;
}

/// Configuration for an inbound connector link (External → AimDB)
///
/// Stores the parsed URL, configuration, and the fused ingest factory. The
/// factory captures the type T at creation time, allowing type-safe
/// deserialize+produce later without needing PhantomData or type parameters.
#[derive(Clone)]
pub struct InboundConnectorLink {
    /// Parsed connector URL
    pub url: ConnectorUrl,

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
    /// Creates a new inbound connector link from a URL and ingest factory
    pub fn new(url: ConnectorUrl, ingest_factory: IngestFactoryFn) -> Self {
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
            .unwrap_or_else(|| self.url.resource_id())
    }
}

/// Configuration for an outbound connector link (AimDB → External)
pub struct OutboundConnectorLink {
    pub url: ConnectorUrl,
    pub config: Vec<(String, String)>,
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
    /// Returns the scheme (e.g., "mqtt", "kafka", "http") that this connector
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
    fn test_topic_provider_type_erasure() {
        use super::{TopicProviderAny, TopicProviderWrapper};

        let provider: Arc<dyn TopicProviderAny> =
            Arc::new(TopicProviderWrapper::new(TestTopicProvider));
        let temp = TestTemperature {
            sensor_id: "kitchen-001".into(),
            celsius: 22.5,
        };

        assert_eq!(
            provider.topic_any(&temp),
            Some("sensors/temp/kitchen-001".into())
        );
    }

    #[test]
    fn test_topic_provider_type_mismatch() {
        use super::{TopicProviderAny, TopicProviderWrapper};

        let provider: Arc<dyn TopicProviderAny> =
            Arc::new(TopicProviderWrapper::new(TestTopicProvider));
        let wrong_type = "not a temperature";

        // Type mismatch returns None (falls back to default topic)
        assert_eq!(provider.topic_any(&wrong_type), None);
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

        use super::{TopicProviderAny, TopicProviderWrapper};

        let provider: Arc<dyn TopicProviderAny> =
            Arc::new(TopicProviderWrapper::new(OptionalTopicProvider));

        // Non-empty sensor_id returns dynamic topic
        let temp_with_id = TestTemperature {
            sensor_id: "abc".into(),
            celsius: 20.0,
        };
        assert_eq!(
            provider.topic_any(&temp_with_id),
            Some("sensors/abc".into())
        );

        // Empty sensor_id returns None (fallback)
        let temp_without_id = TestTemperature {
            sensor_id: String::new(),
            celsius: 20.0,
        };
        assert_eq!(provider.topic_any(&temp_without_id), None);
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
        use super::{ConnectorUrl, InboundConnectorLink};

        let url = ConnectorUrl::parse("mqtt://sensors/temperature").unwrap();
        let link = InboundConnectorLink::new(url, dummy_ingest_factory());

        // No resolver configured, should return static topic from URL
        assert_eq!(link.resolve_topic(), "sensors/temperature");
    }

    #[test]
    fn test_inbound_connector_link_resolve_topic_dynamic() {
        use super::{ConnectorUrl, InboundConnectorLink};

        let url = ConnectorUrl::parse("mqtt://sensors/default").unwrap();
        let mut link = InboundConnectorLink::new(url, dummy_ingest_factory());

        // Configure dynamic resolver
        link.topic_resolver = Some(Arc::new(|| Some("sensors/dynamic/kitchen".into())));

        // Should return resolved topic, not URL topic
        assert_eq!(link.resolve_topic(), "sensors/dynamic/kitchen");
    }

    #[test]
    fn test_inbound_connector_link_resolve_topic_fallback() {
        use super::{ConnectorUrl, InboundConnectorLink};

        let url = ConnectorUrl::parse("mqtt://sensors/fallback").unwrap();
        let mut link = InboundConnectorLink::new(url, dummy_ingest_factory());

        // Configure resolver that returns None
        link.topic_resolver = Some(Arc::new(|| None));

        // Should fall back to static topic from URL
        assert_eq!(link.resolve_topic(), "sensors/fallback");
    }
}
